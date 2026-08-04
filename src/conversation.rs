use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use serde::Serialize;
use tokio::{
    sync::{Mutex, Notify, watch},
    time::{Instant, sleep_until},
};

use crate::compute::ComputeLane;
use crate::continuity::StoredMessage;
use crate::memory::MessageQuote;
use crate::topics::TopicContext;

const QUIET_WINDOW: Duration = Duration::from_millis(1_500);
const TYPING_GRACE: Duration = Duration::from_millis(2_500);
const MAX_SETTLE: Duration = Duration::from_secs(8);
static CONVERSATION_ID: AtomicU64 = AtomicU64::new(1);

/// An external source deliberately attached to one conversational turn.
///
/// It is transient model context: unlike the user's own message, the source
/// payload is not copied into the local conversation archive.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContext {
    pub source: String,
    pub title: String,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct QueuedUserMessage {
    pub text: String,
    pub local_images: Vec<std::path::PathBuf>,
    pub stored: StoredMessage,
    pub reply_to_revision_id: Option<String>,
    pub quotes: Vec<MessageQuote>,
    pub topic: Option<TopicContext>,
    pub external_contexts: Vec<ExternalContext>,
    pub minimum_lane: Option<ComputeLane>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConversationLease {
    id: u64,
}

#[derive(Debug)]
pub enum SettledConversation {
    Messages(Vec<QueuedUserMessage>),
    Interrupted,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSnapshot {
    pub active: bool,
    pub pending_messages: usize,
    pub started_at: Option<String>,
}

#[derive(Clone)]
pub struct ConversationCoordinator {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    input_epoch: watch::Sender<u64>,
}

struct State {
    active: Option<ActiveConversation>,
    typing_until: Option<Instant>,
}

struct ActiveConversation {
    id: u64,
    pending: Vec<QueuedUserMessage>,
    append_reservations: usize,
    interrupted: bool,
    last_input_at: Instant,
    started_at: String,
}

impl ConversationCoordinator {
    pub fn new() -> Self {
        let (input_epoch, _) = watch::channel(0);
        Self {
            state: Arc::new(Mutex::new(State {
                active: None,
                typing_until: None,
            })),
            changed: Arc::new(Notify::new()),
            input_epoch,
        }
    }

    pub async fn start(&self, message: QueuedUserMessage) -> Result<ConversationLease> {
        let mut state = self.state.lock().await;
        if state.active.is_some() {
            anyhow::bail!("a conversation response is already active");
        }
        let id = CONVERSATION_ID.fetch_add(1, Ordering::Relaxed);
        state.active = Some(ActiveConversation {
            id,
            pending: vec![message],
            append_reservations: 0,
            interrupted: false,
            last_input_at: Instant::now(),
            started_at: chrono::Utc::now().to_rfc3339(),
        });
        self.bump_input_epoch();
        self.changed.notify_waiters();
        Ok(ConversationLease { id })
    }

    pub async fn reserve_append(&self) -> Result<ConversationLease> {
        let mut state = self.state.lock().await;
        let active = state
            .active
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no active conversation response"))?;
        if active.interrupted {
            anyhow::bail!("the active conversation response is stopping");
        }
        active.append_reservations += 1;
        Ok(ConversationLease { id: active.id })
    }

    pub async fn append_reserved(
        &self,
        lease: ConversationLease,
        message: QueuedUserMessage,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let active = matching_active_mut(&mut state, lease)?;
        if active.append_reservations == 0 {
            anyhow::bail!("conversation append was not reserved");
        }
        active.append_reservations -= 1;
        active.pending.push(message);
        active.last_input_at = Instant::now();
        self.bump_input_epoch();
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn cancel_append(&self, lease: ConversationLease) {
        let mut state = self.state.lock().await;
        let Ok(active) = matching_active_mut(&mut state, lease) else {
            return;
        };
        active.append_reservations = active.append_reservations.saturating_sub(1);
        self.changed.notify_waiters();
    }

    pub async fn note_typing(&self, typing: bool) {
        let mut state = self.state.lock().await;
        state.typing_until = typing.then(|| Instant::now() + TYPING_GRACE);
        self.changed.notify_waiters();
    }

    pub fn subscribe_input(&self) -> watch::Receiver<u64> {
        self.input_epoch.subscribe()
    }

    /// Creates an input subscription before checking for an active user turn.
    ///
    /// Background workers use this immediately before acquiring the shared
    /// Codex client. If a user starts a turn just after this check, the returned
    /// receiver is already subscribed and will interrupt the background call.
    pub async fn subscribe_background_input(&self) -> Option<watch::Receiver<u64>> {
        let receiver = self.subscribe_input();
        (!self.snapshot().await.active).then_some(receiver)
    }

    pub fn announce_input(&self) {
        self.bump_input_epoch();
    }

    pub fn current_input_epoch(&self) -> u64 {
        *self.input_epoch.borrow()
    }

    pub async fn interrupt(&self) -> bool {
        let interrupted = {
            let mut state = self.state.lock().await;
            let Some(active) = state.active.as_mut() else {
                return false;
            };
            if active.interrupted {
                return false;
            }
            active.interrupted = true;
            active.pending.clear();
            active.append_reservations = 0;
            state.typing_until = None;
            true
        };
        if interrupted {
            self.bump_input_epoch();
            self.changed.notify_waiters();
        }
        interrupted
    }

    pub async fn wait_for_idle_input(&self, expected_epoch: u64, maximum: Duration) -> bool {
        let maximum = Instant::now() + maximum;
        loop {
            if self.current_input_epoch() != expected_epoch {
                return false;
            }
            let deadline = {
                let state = self.state.lock().await;
                if state.active.is_some() {
                    return false;
                }
                state
                    .typing_until
                    .filter(|deadline| *deadline > Instant::now())
                    .unwrap_or_else(Instant::now)
                    .min(maximum)
            };
            if Instant::now() >= deadline {
                return true;
            }
            tokio::select! {
                _ = sleep_until(deadline) => {}
                _ = self.changed.notified() => {}
            }
        }
    }

    pub async fn settle_and_take(&self, lease: ConversationLease) -> Result<SettledConversation> {
        let maximum = Instant::now() + MAX_SETTLE;
        loop {
            let deadline = {
                let state = self.state.lock().await;
                let active = matching_active(&state, lease)?;
                if active.interrupted {
                    return Ok(SettledConversation::Interrupted);
                }
                let quiet = active.last_input_at + QUIET_WINDOW;
                state
                    .typing_until
                    .map(|typing| quiet.max(typing))
                    .unwrap_or(quiet)
                    .min(maximum)
            };
            if Instant::now() >= deadline {
                let mut state = self.state.lock().await;
                let active = matching_active_mut(&mut state, lease)?;
                if active.interrupted {
                    return Ok(SettledConversation::Interrupted);
                }
                if !active.pending.is_empty() {
                    return Ok(SettledConversation::Messages(std::mem::take(
                        &mut active.pending,
                    )));
                }
                anyhow::bail!("conversation batch settled without pending messages");
            }
            tokio::select! {
                _ = sleep_until(deadline) => {}
                _ = self.changed.notified() => {}
            }
        }
    }

    pub async fn has_pending(&self, lease: ConversationLease) -> Result<bool> {
        let state = self.state.lock().await;
        Ok(!matching_active(&state, lease)?.pending.is_empty())
    }

    pub async fn is_interrupted(&self, lease: ConversationLease) -> Result<bool> {
        let state = self.state.lock().await;
        Ok(matching_active(&state, lease)?.interrupted)
    }

    pub async fn finish_if_idle(&self, lease: ConversationLease) -> Result<bool> {
        let mut state = self.state.lock().await;
        let active = matching_active(&state, lease)?;
        if !active.pending.is_empty() || active.append_reservations > 0 {
            return Ok(false);
        }
        state.active = None;
        self.changed.notify_waiters();
        Ok(true)
    }

    pub async fn abort(&self, lease: ConversationLease) {
        let mut state = self.state.lock().await;
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.id == lease.id)
        {
            state.active = None;
            self.changed.notify_waiters();
        }
    }

    pub async fn snapshot(&self) -> ConversationSnapshot {
        let state = self.state.lock().await;
        ConversationSnapshot {
            active: state.active.is_some(),
            pending_messages: state
                .active
                .as_ref()
                .map(|active| active.pending.len() + active.append_reservations)
                .unwrap_or_default(),
            started_at: state
                .active
                .as_ref()
                .map(|active| active.started_at.clone()),
        }
    }

    fn bump_input_epoch(&self) {
        self.input_epoch
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}

fn matching_active(state: &State, lease: ConversationLease) -> Result<&ActiveConversation> {
    state
        .active
        .as_ref()
        .filter(|active| active.id == lease.id)
        .ok_or_else(|| anyhow::anyhow!("conversation lease is no longer active"))
}

fn matching_active_mut(
    state: &mut State,
    lease: ConversationLease,
) -> Result<&mut ActiveConversation> {
    state
        .active
        .as_mut()
        .filter(|active| active.id == lease.id)
        .ok_or_else(|| anyhow::anyhow!("conversation lease is no longer active"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        continuity::StoredMessage,
        memory::{MemoryEntry, MemoryRole},
    };
    use pcp_core::WriteResult;

    fn message(revision: &str) -> QueuedUserMessage {
        QueuedUserMessage {
            text: revision.to_owned(),
            local_images: Vec::new(),
            stored: StoredMessage {
                entry: MemoryEntry {
                    role: MemoryRole::User,
                    at: chrono::Utc::now().to_rfc3339(),
                    content: revision.to_owned(),
                    revision_id: Some(revision.to_owned()),
                    parts: Vec::new(),
                    metadata: None,
                    delivery_state: None,
                },
                page: WriteResult {
                    page_id: format!("page-{revision}"),
                    revision_id: revision.to_owned(),
                    created: true,
                },
                attachment_revision_ids: Vec::new(),
            },
            reply_to_revision_id: None,
            quotes: Vec::new(),
            topic: None,
            external_contexts: Vec::new(),
            minimum_lane: None,
        }
    }

    #[tokio::test]
    async fn appends_messages_to_one_active_response() {
        let coordinator = ConversationCoordinator::new();
        let lease = coordinator.start(message("one")).await.unwrap();
        let reservation = coordinator.reserve_append().await.unwrap();
        coordinator
            .append_reserved(reservation, message("two"))
            .await
            .unwrap();
        let SettledConversation::Messages(batch) =
            coordinator.settle_and_take(lease).await.unwrap()
        else {
            panic!("conversation was unexpectedly interrupted");
        };
        assert_eq!(batch.len(), 2);
        assert!(coordinator.finish_if_idle(lease).await.unwrap());
        assert!(!coordinator.snapshot().await.active);
    }

    #[tokio::test]
    async fn reserved_append_keeps_the_response_open_until_storage_finishes() {
        let coordinator = ConversationCoordinator::new();
        let lease = coordinator.start(message("one")).await.unwrap();
        let SettledConversation::Messages(batch) =
            coordinator.settle_and_take(lease).await.unwrap()
        else {
            panic!("conversation was unexpectedly interrupted");
        };
        assert_eq!(batch.len(), 1);

        let reservation = coordinator.reserve_append().await.unwrap();
        assert!(!coordinator.finish_if_idle(lease).await.unwrap());
        coordinator
            .append_reserved(reservation, message("two"))
            .await
            .unwrap();

        let SettledConversation::Messages(batch) =
            coordinator.settle_and_take(lease).await.unwrap()
        else {
            panic!("conversation was unexpectedly interrupted");
        };
        assert_eq!(batch[0].text, "two");
        assert!(coordinator.finish_if_idle(lease).await.unwrap());
    }

    #[tokio::test]
    async fn interrupt_cancels_a_settling_conversation_and_notifies_turns() {
        let coordinator = ConversationCoordinator::new();
        let mut input_events = coordinator.subscribe_input();
        let lease = coordinator.start(message("one")).await.unwrap();
        input_events.changed().await.unwrap();

        assert!(coordinator.interrupt().await);
        input_events.changed().await.unwrap();
        assert!(coordinator.is_interrupted(lease).await.unwrap());
        assert!(matches!(
            coordinator.settle_and_take(lease).await.unwrap(),
            SettledConversation::Interrupted
        ));
        coordinator.abort(lease).await;
        assert!(!coordinator.snapshot().await.active);
    }

    #[tokio::test]
    async fn input_epoch_notifies_running_turns_of_new_messages() {
        let coordinator = ConversationCoordinator::new();
        let mut input_events = coordinator.subscribe_input();
        let lease = coordinator.start(message("one")).await.unwrap();
        input_events.changed().await.unwrap();
        assert_eq!(*input_events.borrow(), coordinator.current_input_epoch());

        let reservation = coordinator.reserve_append().await.unwrap();
        coordinator
            .append_reserved(reservation, message("two"))
            .await
            .unwrap();
        input_events.changed().await.unwrap();
        assert_eq!(*input_events.borrow(), coordinator.current_input_epoch());
        coordinator.abort(lease).await;
    }

    #[tokio::test]
    async fn background_subscription_observes_or_yields_to_a_user_turn() {
        let coordinator = ConversationCoordinator::new();
        let mut background_input = coordinator
            .subscribe_background_input()
            .await
            .expect("idle background work may subscribe");

        let lease = coordinator.start(message("one")).await.unwrap();
        background_input.changed().await.unwrap();
        assert!(coordinator.subscribe_background_input().await.is_none());

        coordinator.abort(lease).await;
    }
}
