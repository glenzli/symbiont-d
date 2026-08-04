use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::Serialize;
use tokio::{
    sync::{Mutex, mpsc},
    time::sleep,
};
use tracing::warn;

use crate::{
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeStore,
    continuity::{ContinuityHost, MessageLinks},
    conversation::ConversationCoordinator,
    memory::MemoryRole,
    profile::ProfileStore,
    reflection::ReflectionStore,
    usage::UsageStore,
};

const MIN_DELAY_SECONDS: u64 = 5;
const MAX_DELAY_SECONDS: u64 = 90;
const UNARMED_TTL: Duration = Duration::from_secs(120);
static CONTINUATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct ContinuationQueue {
    state: Arc<Mutex<HashMap<String, PendingContinuation>>>,
    sender: mpsc::Sender<ArmedContinuation>,
}

pub type ContinuationReceiver = mpsc::Receiver<ArmedContinuation>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationReservation {
    pub id: String,
    pub reason: String,
    pub delay_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct ArmedContinuation {
    pub id: String,
    pub reason: String,
    pub delay_seconds: u64,
    pub source_assistant_revision_id: String,
    pub input_revision_ids: Vec<String>,
    pub input_epoch: u64,
}

#[derive(Clone)]
struct PendingContinuation {
    reservation: ContinuationReservation,
    created_at: Instant,
    stage: ContinuationStage,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ContinuationStage {
    Reserved,
    Armed,
    Running,
}

impl ContinuationQueue {
    pub fn new() -> (Self, ContinuationReceiver) {
        let (sender, receiver) = mpsc::channel(8);
        (
            Self {
                state: Arc::new(Mutex::new(HashMap::new())),
                sender,
            },
            receiver,
        )
    }

    pub async fn reserve(
        &self,
        reason: &str,
        delay_seconds: u64,
    ) -> Result<ContinuationReservation> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 600 {
            anyhow::bail!("continuation reason must contain 1 to 600 characters");
        }
        if !(MIN_DELAY_SECONDS..=MAX_DELAY_SECONDS).contains(&delay_seconds) {
            anyhow::bail!(
                "continuation delay must be between {MIN_DELAY_SECONDS} and {MAX_DELAY_SECONDS} seconds"
            );
        }

        let mut state = self.state.lock().await;
        state.retain(|_, pending| {
            pending.stage != ContinuationStage::Reserved
                || pending.created_at.elapsed() <= UNARMED_TTL
        });
        if !state.is_empty() {
            anyhow::bail!("one short continuation is already pending");
        }

        let reservation = ContinuationReservation {
            id: next_id(),
            reason: reason.to_owned(),
            delay_seconds,
        };
        state.insert(
            reservation.id.clone(),
            PendingContinuation {
                reservation: reservation.clone(),
                created_at: Instant::now(),
                stage: ContinuationStage::Reserved,
            },
        );
        Ok(reservation)
    }

    pub async fn arm(
        &self,
        reservation_ids: &[String],
        source_assistant_revision_id: String,
        input_revision_ids: Vec<String>,
        input_epoch: u64,
    ) -> Result<Option<String>> {
        let mut state = self.state.lock().await;
        let Some(id) = reservation_ids
            .iter()
            .find(|id| {
                state
                    .get(*id)
                    .is_some_and(|pending| pending.stage == ContinuationStage::Reserved)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let pending = state.get_mut(&id).expect("reservation checked");
        pending.stage = ContinuationStage::Armed;
        let job = ArmedContinuation {
            id: id.clone(),
            reason: pending.reservation.reason.clone(),
            delay_seconds: pending.reservation.delay_seconds,
            source_assistant_revision_id,
            input_revision_ids,
            input_epoch,
        };
        drop(state);

        if self.sender.send(job).await.is_err() {
            self.cancel(&[id]).await;
            anyhow::bail!("continuation worker is unavailable");
        }
        Ok(Some(id))
    }

    pub async fn cancel(&self, reservation_ids: &[String]) {
        let mut state = self.state.lock().await;
        for id in reservation_ids {
            state.remove(id);
        }
    }

    pub async fn cancel_all(&self) {
        self.state.lock().await.clear();
    }

    pub async fn claim(&self, id: &str) -> bool {
        let mut state = self.state.lock().await;
        let Some(pending) = state.get_mut(id) else {
            return false;
        };
        if pending.stage != ContinuationStage::Armed {
            return false;
        }
        pending.stage = ContinuationStage::Running;
        true
    }

    pub async fn is_running(&self, id: &str) -> bool {
        self.state
            .lock()
            .await
            .get(id)
            .is_some_and(|pending| pending.stage == ContinuationStage::Running)
    }

    pub async fn complete(&self, id: &str) {
        self.state.lock().await.remove(id);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_worker(
    mut receiver: ContinuationReceiver,
    queue: Arc<ContinuationQueue>,
    conversation: ConversationCoordinator,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
) {
    tokio::spawn(async move {
        while let Some(job) = receiver.recv().await {
            let queue = Arc::clone(&queue);
            let conversation = conversation.clone();
            let codex = Arc::clone(&codex);
            let compute = Arc::clone(&compute);
            let profile = Arc::clone(&profile);
            let continuity = Arc::clone(&continuity);
            let reflection = Arc::clone(&reflection);
            let usage = Arc::clone(&usage);
            tokio::spawn(async move {
                let id = job.id.clone();
                if let Err(error) = run_continuation(
                    job,
                    Arc::clone(&queue),
                    conversation,
                    codex,
                    compute,
                    profile,
                    continuity,
                    reflection,
                    usage,
                )
                .await
                {
                    warn!(%error, continuation_id = %id, "short continuation failed");
                    queue.complete(&id).await;
                }
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_continuation(
    job: ArmedContinuation,
    queue: Arc<ContinuationQueue>,
    conversation: ConversationCoordinator,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
) -> Result<()> {
    sleep(Duration::from_secs(job.delay_seconds)).await;
    if !conversation
        .wait_for_idle_input(job.input_epoch, Duration::from_secs(20))
        .await
        || !queue.claim(&job.id).await
    {
        queue.complete(&job.id).await;
        return Ok(());
    }
    if !reflection.config().await.continuations_enabled {
        queue.complete(&job.id).await;
        return Ok(());
    }

    let Some(mut input_events) = conversation.subscribe_background_input().await else {
        queue.complete(&job.id).await;
        return Ok(());
    };
    input_events.borrow_and_update();
    if conversation.current_input_epoch() != job.input_epoch {
        queue.complete(&job.id).await;
        return Ok(());
    }
    let compute = compute.snapshot().await;
    let profile = profile.snapshot().await;
    let continuity_context = "This is a short continuation of the immediately preceding conversation. Use the native \
         thread or the supplied exact working-context bridge; do not perform broad recall.";
    let Ok(mut client) = codex.try_lock() else {
        queue.complete(&job.id).await;
        return Ok(());
    };
    let (events, mut runtime_events) = mpsc::channel::<RuntimeEvent>(64);
    let event_drain = tokio::spawn(async move { while runtime_events.recv().await.is_some() {} });
    let mut outcome = client
        .continue_conversation(
            &job.reason,
            &compute,
            &profile,
            continuity_context,
            input_events,
            events,
        )
        .await?;
    drop(client);
    event_drain.await?;

    let publish = !outcome.interrupted
        && !outcome.text.trim().is_empty()
        && conversation.current_input_epoch() == job.input_epoch
        && queue.is_running(&job.id).await;
    if !publish {
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
    }
    usage.record_all(&outcome.invocations).await?;
    if publish {
        let mut input_revision_ids = job.input_revision_ids;
        input_revision_ids.extend(outcome.context_revision_ids);
        input_revision_ids.sort();
        input_revision_ids.dedup();
        let stored = continuity
            .ingest_message(
                MemoryRole::Assistant,
                &outcome.text,
                Vec::new(),
                Some(outcome.metadata),
                MessageLinks {
                    responds_to: None,
                    continues_from: Some(job.source_assistant_revision_id.clone()),
                    input_revision_ids,
                    surfaced_hunch_revision_ids: Vec::new(),
                    quotes: Vec::new(),
                    topic: None,
                },
            )
            .await?;
        codex
            .lock()
            .await
            .mark_interactive_revision(stored.page.revision_id.clone());
        reflection
            .record_message(
                &stored.entry,
                Some(&job.source_assistant_revision_id),
                false,
                &[],
            )
            .await?;
    }
    queue.complete(&job.id).await;
    Ok(())
}

fn next_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = CONTINUATION_ID.fetch_add(1, Ordering::Relaxed);
    format!("continuation-{timestamp}-{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permits_only_one_live_reservation() {
        let (queue, _receiver) = ContinuationQueue::new();
        let first = queue
            .reserve("A distinct association may still matter.", 8)
            .await
            .expect("reserve");

        assert!(queue.reserve("Another one.", 8).await.is_err());
        queue.cancel(&[first.id]).await;
        assert!(queue.reserve("Replacement.", 8).await.is_ok());
    }

    #[tokio::test]
    async fn canceled_reservation_cannot_be_armed() {
        let (queue, mut receiver) = ContinuationQueue::new();
        let reservation = queue
            .reserve("Reconsider this after a short pause.", 8)
            .await
            .expect("reserve");
        queue.cancel_all().await;

        let armed = queue
            .arm(
                &[reservation.id],
                "assistant-revision".to_owned(),
                Vec::new(),
                1,
            )
            .await
            .expect("arm");

        assert!(armed.is_none());
        assert!(receiver.try_recv().is_err());
    }
}
