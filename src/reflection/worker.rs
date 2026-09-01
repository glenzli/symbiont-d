use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use tokio::{
    sync::{Mutex, RwLock, mpsc},
    time::{Instant, sleep, sleep_until},
};
use tracing::{debug, warn};

use super::{
    HunchFeedbackTarget, InteractionEvent, ReflectionConfig, ReflectionPhase, ReflectionRuntime,
    ReflectionSnapshot, ReflectionStore,
};
use crate::{
    autonomy::AutonomyStore,
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeStore,
    continuity::{ContinuityHost, MessageLinks},
    conversation::ConversationCoordinator,
    curiosity::CuriosityStore,
    exploration::{ExplorationHandle, quiet_end, today_started_at},
    memory::{MemoryEntry, MemoryRole, MessageMetadata},
    outreach::{OutreachCandidate, has_budget},
    pcp_index::PcpIndex,
    profile::{ProfileStore, SetupStatus},
    symbiont_context::SymbiontContextStore,
    usage::UsageStore,
};

const PERIODIC_CHECK: Duration = Duration::from_secs(30);
const BUSY_RETRY: Duration = Duration::from_secs(20);
const MAX_BATCH_EVENTS: usize = 24;

#[derive(Clone, Copy, Debug)]
enum ReflectionTrigger {
    Conversation,
    Manual,
    Startup,
}

impl ReflectionTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Manual => "manual",
            Self::Startup => "startup",
        }
    }
}

#[derive(Clone)]
pub struct ReflectionHandle {
    store: Arc<ReflectionStore>,
    runtime: Arc<RwLock<ReflectionRuntime>>,
    trigger: mpsc::Sender<ReflectionTrigger>,
}

impl ReflectionHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        store: Arc<ReflectionStore>,
        pcp_index: Arc<PcpIndex>,
        autonomy: Arc<AutonomyStore>,
        profile: Arc<ProfileStore>,
        codex: Arc<Mutex<CodexClient>>,
        compute: Arc<ComputeStore>,
        continuity: Arc<ContinuityHost>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        usage: Arc<UsageStore>,
        conversation: ConversationCoordinator,
        exploration: ExplorationHandle,
    ) -> Self {
        let runtime = Arc::new(RwLock::new(ReflectionRuntime::default()));
        let (trigger, trigger_rx) = mpsc::channel(32);
        tokio::spawn(run(
            Arc::clone(&store),
            Arc::clone(&runtime),
            pcp_index,
            autonomy,
            profile,
            codex,
            compute,
            continuity,
            context,
            curiosity,
            usage,
            conversation,
            exploration,
            trigger_rx,
        ));
        let handle = Self {
            store,
            runtime,
            trigger,
        };
        let _ = handle.trigger.try_send(ReflectionTrigger::Startup);
        handle
    }

    pub fn store(&self) -> &Arc<ReflectionStore> {
        &self.store
    }

    pub async fn snapshot(&self) -> Result<ReflectionSnapshot> {
        let config = self.store.config().await;
        let runtime = self.runtime().await;
        Ok(ReflectionSnapshot {
            config,
            runtime,
            health: self.store.projection_health().await?,
            episodes: self.store.episodes(30).await?,
            hypotheses: self.store.hypotheses(40).await?,
            follow_ups: self.store.follow_ups(30).await?,
            recent_runs: self.store.recent_runs(20).await?,
        })
    }

    pub async fn runtime(&self) -> ReflectionRuntime {
        let config = self.store.config().await;
        let mut runtime = self.runtime.read().await.clone();
        runtime.pending_events = self.store.pending_count().await.unwrap_or_default();
        if runtime.pending_events == 0 && runtime.phase != ReflectionPhase::Reflecting {
            runtime.next_run_at = None;
        } else if runtime.pending_events > 0
            && runtime.next_run_at.is_none()
            && runtime.phase != ReflectionPhase::Reflecting
        {
            runtime.next_run_at = Some(after_seconds(config.settle_seconds));
        }
        if runtime.last_run_at.is_none()
            && let Ok(runs) = self.store.recent_runs(1).await
            && let Some(run) = runs.first()
        {
            runtime.last_run_at = run
                .completed_at
                .clone()
                .or_else(|| Some(run.started_at.clone()));
            runtime.last_summary = run.summary.clone();
            runtime.last_error = run.error.clone();
            if run.status == "error" && runtime.phase == ReflectionPhase::Waiting {
                runtime.phase = ReflectionPhase::Error;
            }
        }
        runtime
    }

    pub async fn update_config(&self, config: ReflectionConfig) -> Result<ReflectionConfig> {
        let config = self.store.update_config(config).await?;
        self.refresh_pending_runtime().await;
        let _ = self.trigger.try_send(ReflectionTrigger::Conversation);
        Ok(config)
    }

    pub fn trigger(&self) -> bool {
        self.trigger.try_send(ReflectionTrigger::Manual).is_ok()
    }

    pub async fn record_message(
        &self,
        entry: &MemoryEntry,
        related_revision_id: Option<&str>,
        hunch_feedback: &[HunchFeedbackTarget],
    ) -> Result<()> {
        self.store
            .record_message(entry, related_revision_id, false, hunch_feedback)
            .await?;
        let should_trigger = self.store.pending_count().await? > 0;
        self.refresh_pending_runtime().await;
        if should_trigger {
            let _ = self.trigger.try_send(ReflectionTrigger::Conversation);
        }
        Ok(())
    }

    pub async fn record_turn_disposition(
        &self,
        revision_id: &str,
        reaction: Option<&str>,
    ) -> Result<()> {
        self.store
            .record_turn_disposition(revision_id, reaction)
            .await?;
        self.refresh_pending_runtime().await;
        let _ = self.trigger.try_send(ReflectionTrigger::Conversation);
        Ok(())
    }

    pub async fn record_seen(&self, revision_ids: Vec<String>, occurred_at: String) -> Result<()> {
        self.store.record_seen(revision_ids, occurred_at).await?;
        self.refresh_pending_runtime().await;
        Ok(())
    }

    pub async fn record_retraction(&self, revision_ids: &[String]) -> Result<()> {
        self.store.record_retraction(revision_ids).await?;
        self.refresh_pending_runtime().await;
        let _ = self.trigger.try_send(ReflectionTrigger::Conversation);
        Ok(())
    }

    async fn refresh_pending_runtime(&self) {
        let config = self.store.config().await;
        let pending_events = self.store.pending_count().await.unwrap_or_default();
        let mut runtime = self.runtime.write().await;
        runtime.pending_events = pending_events;
        if !config.enabled {
            runtime.phase = ReflectionPhase::Disabled;
            runtime.next_run_at = None;
        } else if runtime.phase != ReflectionPhase::Reflecting {
            runtime.phase = ReflectionPhase::Waiting;
            runtime.next_run_at =
                (pending_events > 0).then(|| after_seconds(config.settle_seconds));
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    store: Arc<ReflectionStore>,
    runtime: Arc<RwLock<ReflectionRuntime>>,
    pcp_index: Arc<PcpIndex>,
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    usage: Arc<UsageStore>,
    conversation: ConversationCoordinator,
    exploration: ExplorationHandle,
    mut trigger_rx: mpsc::Receiver<ReflectionTrigger>,
) {
    sleep(Duration::from_secs(5)).await;
    let mut pending_trigger = Some(ReflectionTrigger::Startup);

    loop {
        let mut trigger = if let Some(trigger) = pending_trigger.take() {
            trigger
        } else {
            tokio::select! {
                trigger = trigger_rx.recv() => {
                    let Some(trigger) = trigger else {
                        break;
                    };
                    trigger
                }
                _ = sleep(PERIODIC_CHECK) => {
                    check_deferred_follow_ups(
                        &store, &autonomy, &profile, &exploration
                    ).await;
                    ReflectionTrigger::Conversation
                }
            }
        };

        if !matches!(trigger, ReflectionTrigger::Manual) {
            let settle = store.config().await.settle_seconds;
            let mut deadline = Instant::now() + Duration::from_secs(settle as u64);
            set_settle_runtime(&store, &runtime, settle).await;
            loop {
                tokio::select! {
                    _ = sleep_until(deadline) => break,
                    next = trigger_rx.recv() => {
                        let Some(next) = next else {
                            return;
                        };
                        if matches!(next, ReflectionTrigger::Manual) {
                            trigger = ReflectionTrigger::Manual;
                            break;
                        }
                        deadline = Instant::now() + Duration::from_secs(settle as u64);
                        set_settle_runtime(&store, &runtime, settle).await;
                    }
                }
            }
        }

        check_deferred_follow_ups(&store, &autonomy, &profile, &exploration).await;
        match reflect_once(
            &store,
            &runtime,
            &pcp_index,
            &autonomy,
            &profile,
            &codex,
            &compute,
            &continuity,
            &context,
            &curiosity,
            &usage,
            &conversation,
            trigger,
        )
        .await
        {
            Ok(ReflectState::Completed) => {
                if store.pending_count().await.unwrap_or_default() > 0 {
                    pending_trigger = Some(ReflectionTrigger::Conversation);
                }
            }
            Ok(ReflectState::Busy) => {
                sleep(BUSY_RETRY).await;
                pending_trigger = Some(trigger);
            }
            Ok(ReflectState::Interrupted) => {
                pending_trigger = Some(ReflectionTrigger::Conversation);
            }
            Ok(ReflectState::Idle) => {}
            Err(error) => {
                warn!(%error, "interaction Reflection failed");
                let mut current = runtime.write().await;
                current.phase = ReflectionPhase::Error;
                current.last_error = Some(error.to_string());
                current.current_activity = None;
            }
        }
    }
}

enum ReflectState {
    Completed,
    Busy,
    Interrupted,
    Idle,
}

#[allow(clippy::too_many_arguments)]
async fn reflect_once(
    store: &ReflectionStore,
    runtime: &Arc<RwLock<ReflectionRuntime>>,
    pcp_index: &PcpIndex,
    autonomy: &AutonomyStore,
    profile: &ProfileStore,
    codex: &Mutex<CodexClient>,
    compute: &ComputeStore,
    continuity: &ContinuityHost,
    context: &SymbiontContextStore,
    curiosity: &CuriosityStore,
    usage: &UsageStore,
    conversation: &ConversationCoordinator,
    trigger: ReflectionTrigger,
) -> Result<ReflectState> {
    let config = store.config().await;
    let autonomy_config = autonomy.snapshot().await;
    let profile_snapshot = profile.snapshot().await;
    let pending_events = store.pending_count().await?;
    {
        let mut current = runtime.write().await;
        current.pending_events = pending_events;
        current.next_run_at = None;
        current.last_error = None;
    }
    if !config.enabled {
        runtime.write().await.phase = ReflectionPhase::Disabled;
        return Ok(ReflectState::Idle);
    }
    if profile_snapshot.status != SetupStatus::Ready {
        runtime.write().await.phase = ReflectionPhase::NeedsSetup;
        return Ok(ReflectState::Idle);
    }
    let headline = usage.headline(&today_started_at()).await?;
    if config.daily_token_limit > 0 && headline.reflection_tokens_today >= config.daily_token_limit
    {
        runtime.write().await.phase = ReflectionPhase::TokenLimit;
        return Ok(ReflectState::Idle);
    }
    let batch = match store.pending_batch(MAX_BATCH_EVENTS).await? {
        Some(batch) => batch,
        None if matches!(trigger, ReflectionTrigger::Manual)
            || store.lifecycle_review_due().await? =>
        {
            let Some(batch) = store.lifecycle_batch().await? else {
                runtime.write().await.phase = ReflectionPhase::Waiting;
                return Ok(ReflectState::Idle);
            };
            batch
        }
        None => {
            runtime.write().await.phase = ReflectionPhase::Waiting;
            return Ok(ReflectState::Idle);
        }
    };
    let compound_bundle = match compound_recurrence_bundle(&batch.events, continuity).await {
        Ok(bundle) => bundle,
        Err(error) => {
            warn!(%error, "could not assemble compound recurrence evidence");
            None
        }
    };
    let source_bundle = match compound_bundle {
        Some(compound) => format!(
            "{}\n\n<pcp-compound-promotion-context>\n{}\n</pcp-compound-promotion-context>",
            batch.source_bundle, compound
        ),
        None => batch.source_bundle.clone(),
    };
    let Some(input_events) = conversation.subscribe_background_input().await else {
        let mut current = runtime.write().await;
        current.phase = ReflectionPhase::Waiting;
        current.current_activity = None;
        return Ok(ReflectState::Busy);
    };
    let input_epoch = *input_events.borrow();
    let Ok(mut client) = codex.try_lock() else {
        runtime.write().await.current_activity = Some("等待当前模型调用完成".to_owned());
        return Ok(ReflectState::Busy);
    };

    let run_id = store
        .start_run(
            if batch.lifecycle_audit {
                "lifecycle"
            } else {
                trigger.as_str()
            },
            &batch,
        )
        .await?;
    if batch.lifecycle_audit {
        store.mark_lifecycle_reviewed().await?;
    }
    {
        let mut current = runtime.write().await;
        current.phase = ReflectionPhase::Reflecting;
        current.current_activity = Some("正在理解近期对话".to_owned());
    }
    let compute = compute.snapshot().await;
    let continuity_context = format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}",
        continuity.context_seed(None).await,
        context.prompt().await?,
        curiosity.prompt().await?,
        store.prompt().await?,
        autonomy_config.attention_context(),
    );
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let runtime_for_events = runtime.clone();
    let event_drain = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if let RuntimeEvent::Activity { label, .. } = event {
                runtime_for_events.write().await.current_activity = Some(label);
            }
        }
    });
    let outcome = client
        .reflect_interaction(
            &source_bundle,
            &compute,
            &profile_snapshot,
            &continuity_context,
            input_events,
            events_tx,
        )
        .await;
    drop(client);
    event_drain.await.context("join Reflection event drain")?;

    let mut outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            store.fail_run(&run_id, &error.to_string()).await?;
            return Err(error);
        }
    };
    if outcome.interrupted {
        usage.record_all(&outcome.invocations).await?;
        store
            .interrupt_run(&run_id, "superseded_by_user_input")
            .await?;
        let mut current = runtime.write().await;
        current.phase = ReflectionPhase::Waiting;
        current.last_run_at = Some(now());
        current.last_summary = None;
        current.last_error = None;
        current.current_activity = None;
        current.pending_events = store.pending_count().await?;
        return Ok(ReflectState::Interrupted);
    }
    let feedback_page_ids = batch
        .events
        .iter()
        .filter_map(|event| event.payload.get("hunchFeedback"))
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .filter_map(|target| target.get("pageId"))
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let unreconciled = curiosity
        .pending_feedback_page_ids(&feedback_page_ids)
        .await?;
    if !unreconciled.is_empty() {
        let message = format!(
            "Reflection left Hunch feedback pending for {}",
            unreconciled.join(", ")
        );
        store.fail_run(&run_id, &message).await?;
        anyhow::bail!("{message}");
    }
    let outreach_published = if let Some(outreach) = outcome.outreach.as_ref() {
        match publish_outreach(
            store,
            autonomy,
            usage,
            continuity,
            conversation,
            input_epoch,
            outreach,
            &outcome.metadata,
            &outcome.context_revision_ids,
        )
        .await
        {
            Ok(published) => published,
            Err(error) => {
                warn!(%error, "could not publish Reflection outreach");
                false
            }
        }
    } else {
        false
    };
    if outreach_published && let Some(invocation) = outcome.invocations.last_mut() {
        invocation.produced_message = true;
    }
    if outcome.outreach.is_some() {
        outcome.actions.push(
            if outreach_published {
                "outreach.published"
            } else {
                "outreach.suppressed"
            }
            .to_owned(),
        );
    }
    if batch.lifecycle_audit {
        outcome.actions.push("lifecycle.reviewed".to_owned());
    }
    usage.record_all(&outcome.invocations).await?;
    let trace_id = outcome
        .invocations
        .last()
        .map(|invocation| invocation.id.clone());
    let model = outcome
        .invocations
        .last()
        .map(|invocation| invocation.effective_model.clone());
    let total_tokens = outcome
        .invocations
        .iter()
        .map(|invocation| invocation.total_tokens)
        .sum();
    store
        .complete_run(
            &run_id,
            outcome.summary.clone(),
            trace_id,
            model,
            total_tokens,
            outcome.actions,
            batch.to_event_id,
        )
        .await?;
    if let Err(error) = pcp_index.sync_all().await {
        warn!(%error, "could not refresh the PCP model-written index after Reflection");
    }
    store.prune().await?;
    {
        let mut current = runtime.write().await;
        current.phase = ReflectionPhase::Waiting;
        current.last_run_at = Some(now());
        current.last_summary = outcome.summary;
        current.last_error = None;
        current.current_activity = None;
        current.pending_events = store.pending_count().await?;
    }
    debug!(
        run_id,
        events = batch.events.len(),
        "interaction Reflection completed"
    );
    Ok(ReflectState::Completed)
}

async fn compound_recurrence_bundle(
    events: &[InteractionEvent],
    continuity: &ContinuityHost,
) -> Result<Option<String>> {
    let Some(query) = events.iter().rev().find_map(|event| {
        if event.retracted
            || event.role.as_deref() != Some("user")
            || event
                .payload
                .get("imported")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        {
            return None;
        }
        event
            .payload
            .get("excerpt")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| value.chars().count() >= 4)
            .map(|value| value.chars().take(512).collect::<String>())
    }) else {
        return Ok(None);
    };
    let context = continuity.compound_context(&query, &[]).await?;
    if !context.has_meaningful_recurrence() {
        return Ok(None);
    }
    Ok(Some(context.prompt()))
}

#[allow(clippy::too_many_arguments)]
async fn publish_outreach(
    store: &ReflectionStore,
    autonomy: &AutonomyStore,
    usage: &UsageStore,
    continuity: &ContinuityHost,
    conversation: &ConversationCoordinator,
    input_epoch: u64,
    outreach: &OutreachCandidate,
    metadata: &MessageMetadata,
    context_revision_ids: &[String],
) -> Result<bool> {
    let reflection_config = store.config().await;
    if !reflection_config.proactive_messages_enabled || !autonomy.permitted(true).await {
        return Ok(false);
    }
    let autonomy_config = autonomy.snapshot().await;
    let headline = usage.headline(&today_started_at()).await?;
    if !has_budget(outreach.kind, &autonomy_config, &headline)
        || quiet_end(Utc::now(), &autonomy_config.quiet_hours).is_some()
        || !conversation
            .wait_for_idle_input(input_epoch, Duration::ZERO)
            .await
    {
        return Ok(false);
    }

    let mut input_revision_ids = outreach.source_revision_ids.clone();
    input_revision_ids.extend_from_slice(context_revision_ids);
    input_revision_ids.sort();
    input_revision_ids.dedup();
    let stored = continuity
        .ingest_message(
            MemoryRole::Assistant,
            &outreach.message,
            Vec::new(),
            Some(metadata.clone()),
            MessageLinks {
                responds_to: None,
                continues_from: None,
                input_revision_ids,
                surfaced_hunch_revision_ids: Vec::new(),
                quotes: Vec::new(),
                topic: None,
            },
        )
        .await?;
    store
        .record_message(&stored.entry, None, false, &[])
        .await?;
    debug!(
        reason = %outreach.reason,
        revision_id = ?stored.entry.revision_id,
        "published Reflection outreach"
    );
    Ok(true)
}

async fn check_deferred_follow_ups(
    store: &ReflectionStore,
    autonomy: &AutonomyStore,
    profile: &ProfileStore,
    exploration: &ExplorationHandle,
) {
    let config = store.config().await;
    if !config.enabled || !config.follow_ups_enabled {
        return;
    }
    let initialized = profile.snapshot().await.status == SetupStatus::Ready;
    if !autonomy.permitted(initialized).await {
        return;
    }
    let due = match store.due_follow_ups().await {
        Ok(due) => due,
        Err(error) => {
            warn!(%error, "could not inspect deferred follow-ups");
            return;
        }
    };
    if due.is_empty() || !exploration.trigger_follow_up() {
        return;
    }
    let ids = due
        .into_iter()
        .map(|follow_up| follow_up.id)
        .collect::<Vec<_>>();
    if let Err(error) = store.mark_follow_ups_triggered(&ids).await {
        warn!(%error, "could not mark deferred follow-ups as triggered");
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn after_seconds(seconds: u32) -> String {
    (Utc::now() + chrono::Duration::seconds(seconds as i64))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn set_settle_runtime(
    store: &ReflectionStore,
    runtime: &RwLock<ReflectionRuntime>,
    settle_seconds: u32,
) {
    let pending_events = store.pending_count().await.unwrap_or_default();
    let mut current = runtime.write().await;
    if current.phase != ReflectionPhase::Reflecting {
        current.phase = ReflectionPhase::Waiting;
    }
    current.pending_events = pending_events;
    current.next_run_at = Some(after_seconds(settle_seconds));
}
