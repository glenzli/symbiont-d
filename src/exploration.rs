mod attempt_log;
mod intent;
mod manual_run;
mod sensing_route;

pub use attempt_log::{ExplorationAttemptStore, ExplorationSkippedAttempt};
pub use intent::{
    ExplorationIntent, ExplorationIntentOrigin, ExplorationIntentQueue, ExplorationIntentReceiver,
    ExplorationIntentStatus, NewExplorationIntent,
};
pub use manual_run::{ManualExplorationRun, ManualExplorationStatus, ManualExplorationStore};

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tokio::{
    sync::{Mutex, RwLock, mpsc},
    task::{JoinHandle, JoinSet},
    time::sleep,
};

use crate::{
    ambient_api::{AmbientInput, AmbientScout},
    autonomy::{AutonomyConfig, AutonomyStore, QuietHours},
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeStore,
    continuity::{ContinuityHost, MessageLinks},
    conversation::ConversationCoordinator,
    curiosity::CuriosityStore,
    drive_input::DriveInputStore,
    external_markdown::source_urls,
    inference::{InferenceAttempt, InferenceExecutor, hard_deduplicate},
    luna_input::LunaInput,
    mail_input::MailInputStore,
    memory::{MemoryEntry, MemoryRole},
    outreach::all_budgets_exhausted,
    profile::{ProfileStore, SetupStatus},
    reflection::ReflectionStore,
    sensing::{
        SensingCandidate, SensingDeduplicationReference, SensingIntakeBrief, SensingStore,
        format_candidate_pool,
    },
    signals::{SignalPublishOutcome, SignalStore},
    symbiont_context::SymbiontContextStore,
    usage::{UsageHeadline, UsageStore},
};

use self::sensing_route::{
    SensingDeliveryMetrics, annotate_sensing_delivery, link_sensing_invocations,
    plan_sensing_routes, prioritize_candidate_batches, sensing_review_batch_size,
};

const POLICY_REFRESH: Duration = Duration::from_secs(30);
const EXPLORATION_CHAT_TAIL: usize = 14;
const EXPLORATION_JOURNAL_RUNS: usize = 8;
const EXPLORATION_CONTEXT_CHARS: usize = 16_000;
const EXPLORATION_MESSAGE_EXCERPT_CHARS: usize = 700;
const EXPLORATION_EDGE_EXCERPT_CHARS: usize = 900;
const SENSING_CHAT_TAIL: usize = 2;
const SENSING_MESSAGE_EXCERPT_CHARS: usize = 320;
const SENSING_SOURCE_CHAT_TAIL: usize = 64;
const MAX_SENSING_DEDUPLICATION_REFERENCES: usize = 32;
const MAX_SENSING_LEDGER_REFERENCES: usize = 12;
const SENSING_REFERENCE_EXCERPT_CHARS: usize = 480;
const RETRY_DELAY: Duration = Duration::from_secs(2);
static MANUAL_RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationPhase {
    Disabled,
    NeedsSetup,
    Waiting,
    QuietHours,
    TokenLimit,
    MessageLimit,
    Exploring,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationActivity {
    pub label: String,
    pub model: String,
    pub display_name: String,
    pub effort: String,
    pub lane: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationSnapshot {
    pub phase: ExplorationPhase,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_outcome: Option<String>,
    pub last_error: Option<String>,
    pub current_activity: Option<ExplorationActivity>,
    pub latest_message: Option<MemoryEntry>,
    pub current_trigger: Option<String>,
    pub last_trigger: Option<String>,
    pub last_skipped_attempt: Option<ExplorationSkippedAttempt>,
    pub pending_candidate_count: usize,
    pub current_review_candidate_count: usize,
    pub last_reviewed_candidate_count: usize,
    pub manual_run: Option<ManualExplorationRun>,
    pub manual_receipts: Vec<ManualExplorationRun>,
}

impl Default for ExplorationSnapshot {
    fn default() -> Self {
        Self {
            phase: ExplorationPhase::Disabled,
            next_run_at: None,
            last_run_at: None,
            last_outcome: None,
            last_error: None,
            current_activity: None,
            latest_message: None,
            current_trigger: None,
            last_trigger: None,
            last_skipped_attempt: None,
            pending_candidate_count: 0,
            current_review_candidate_count: 0,
            last_reviewed_candidate_count: 0,
            manual_run: None,
            manual_receipts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
enum ExplorationTrigger {
    Manual {
        request_id: String,
        requested_at: String,
        bypass_token_limit: bool,
    },
    DeferredFollowUp,
    Intent(ExplorationIntent),
}

impl ExplorationTrigger {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Manual { .. } => "manual",
            Self::DeferredFollowUp => "deferred_follow_up",
            Self::Intent(_) => "thought_intent",
        }
    }

    fn preparation_label(&self) -> &'static str {
        match self {
            Self::Manual { .. } => "准备主动探索",
            Self::DeferredFollowUp => "准备重新看看之前留下的话题",
            Self::Intent(_) => "准备跟进一个刚产生的探索问题",
        }
    }

    fn bypasses_quiet_hours(&self) -> bool {
        matches!(self, Self::Manual { .. })
    }

    fn bypasses_token_limit(&self) -> bool {
        matches!(
            self,
            Self::Manual {
                bypass_token_limit: true,
                ..
            }
        )
    }

    fn bypasses_message_limit(&self) -> bool {
        matches!(self, Self::Manual { .. })
    }

    fn intent(&self) -> Option<&ExplorationIntent> {
        match self {
            Self::Intent(intent) => Some(intent),
            _ => None,
        }
    }

    fn manual_request(&self) -> Option<(&str, &str)> {
        match self {
            Self::Manual {
                request_id,
                requested_at,
                ..
            } => Some((request_id, requested_at)),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct ExplorationHandle {
    state: Arc<RwLock<ExplorationSnapshot>>,
    trigger: mpsc::Sender<ExplorationTrigger>,
    intents: Arc<ExplorationIntentQueue>,
    sensing: Arc<SensingStore>,
    manual_runs: Arc<ManualExplorationStore>,
    attempts: Arc<ExplorationAttemptStore>,
}

impl ExplorationHandle {
    pub async fn start(
        autonomy: Arc<AutonomyStore>,
        profile: Arc<ProfileStore>,
        codex: Arc<Mutex<CodexClient>>,
        inference: Arc<InferenceExecutor>,
        compute: Arc<ComputeStore>,
        continuity: Arc<ContinuityHost>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        usage: Arc<UsageStore>,
        ambient_scout: Arc<AmbientScout>,
        luna_input: Arc<LunaInput>,
        drive_input: Arc<DriveInputStore>,
        mail_input: Arc<MailInputStore>,
        sensing: Arc<SensingStore>,
        signals: Arc<SignalStore>,
        conversation: ConversationCoordinator,
        intents: Arc<ExplorationIntentQueue>,
        manual_runs: Arc<ManualExplorationStore>,
        attempts: Arc<ExplorationAttemptStore>,
        mut intent_receiver: ExplorationIntentReceiver,
    ) -> Self {
        let projection = manual_runs.projection().await;
        if ambient_scout.has_configured_input().await
            || drive_input.has_configured_input().await
            || mail_input.has_configured_input().await
        {
            if let Err(error) = attempts.remove_reason("no_input_channel").await {
                tracing::warn!(%error, "could not clear stale input-configuration skips");
            }
        }
        let last_skipped_attempt = attempts.latest().await;
        let pending_candidate_count = sensing.count().await.unwrap_or_default();
        let state = Arc::new(RwLock::new(ExplorationSnapshot {
            manual_run: projection.latest,
            manual_receipts: projection.unpresented,
            last_skipped_attempt,
            pending_candidate_count,
            ..ExplorationSnapshot::default()
        }));
        let (trigger, trigger_rx) = mpsc::channel(32);
        let intent_trigger = trigger.clone();
        let intent_store = Arc::clone(&intents);
        tokio::spawn(async move {
            while let Some(id) = intent_receiver.recv().await {
                let trigger = intent_trigger.clone();
                let store = Arc::clone(&intent_store);
                tokio::spawn(async move {
                    let Some(intent) = store.get(&id).await else {
                        return;
                    };
                    if intent.status != ExplorationIntentStatus::Queued {
                        return;
                    }
                    if let Ok(not_before) = DateTime::parse_from_rfc3339(&intent.not_before) {
                        let wait = (not_before.with_timezone(&Utc) - Utc::now())
                            .to_std()
                            .unwrap_or(Duration::ZERO);
                        sleep(wait).await;
                    }
                    let Some(intent) = store.get(&id).await else {
                        return;
                    };
                    if intent.status == ExplorationIntentStatus::Queued {
                        let _ = trigger.send(ExplorationTrigger::Intent(intent)).await;
                    }
                });
            }
        });
        tokio::spawn(run_scheduler(
            Arc::clone(&state),
            autonomy,
            profile,
            codex,
            inference,
            compute,
            continuity,
            context,
            curiosity,
            reflection,
            usage,
            ambient_scout,
            luna_input,
            drive_input,
            mail_input,
            Arc::clone(&sensing),
            Arc::clone(&signals),
            conversation,
            Arc::clone(&intents),
            Arc::clone(&manual_runs),
            Arc::clone(&attempts),
            trigger_rx,
        ));
        Self {
            state,
            trigger,
            intents,
            sensing,
            manual_runs,
            attempts,
        }
    }

    pub async fn snapshot(&self) -> ExplorationSnapshot {
        self.state.read().await.clone()
    }

    pub async fn candidates(&self) -> Result<Vec<SensingCandidate>> {
        self.sensing.candidates().await
    }

    pub async fn trigger(&self, bypass_token_limit: bool) -> Result<Option<String>> {
        let requested_at = timestamp(Utc::now());
        let request_id = format!(
            "explore_{:x}_{:x}",
            Utc::now().timestamp_micros(),
            MANUAL_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        if self
            .manual_runs
            .accept(request_id.clone(), requested_at.clone())
            .await?
            .is_none()
        {
            return Ok(None);
        }
        refresh_manual_projection(&self.state, &self.manual_runs).await;
        let trigger = ExplorationTrigger::Manual {
            request_id: request_id.clone(),
            requested_at: requested_at.clone(),
            bypass_token_limit,
        };
        if self.trigger.try_send(trigger).is_err() {
            self.manual_runs
                .fail(&request_id, "scheduler_unavailable")
                .await?;
            refresh_manual_projection(&self.state, &self.manual_runs).await;
            anyhow::bail!("exploration scheduler is unavailable");
        }
        tracing::info!(
            target: crate::runtime_log::TARGET,
            event = "manual_exploration_accepted",
            request_id,
            "manual exploration was queued"
        );
        Ok(Some(request_id))
    }

    pub async fn acknowledge_manual_receipt(
        &self,
        request_id: &str,
    ) -> Result<Option<ManualExplorationRun>> {
        let receipt = self.manual_runs.acknowledge(request_id).await?;
        refresh_manual_projection(&self.state, &self.manual_runs).await;
        Ok(receipt)
    }

    pub fn trigger_follow_up(&self) -> bool {
        self.trigger
            .try_send(ExplorationTrigger::DeferredFollowUp)
            .is_ok()
    }

    pub async fn recent_intents(&self, limit: usize) -> Vec<ExplorationIntent> {
        self.intents.recent(limit).await
    }

    pub async fn recent_skipped_attempts(&self, limit: usize) -> Vec<ExplorationSkippedAttempt> {
        self.attempts.recent(limit).await
    }

    pub async fn clear_stale_input_configuration_skips(&self) -> Result<()> {
        self.attempts.remove_reason("no_input_channel").await?;
        let mut state = self.state.write().await;
        if state
            .last_skipped_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.reason == "no_input_channel")
        {
            state.last_skipped_attempt = None;
        }
        Ok(())
    }

    pub async fn supersede_intents(&self, ids: &[String], reason: &str) -> Result<()> {
        self.intents.supersede(ids, reason).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_scheduler(
    state: Arc<RwLock<ExplorationSnapshot>>,
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    inference: Arc<InferenceExecutor>,
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
    ambient_scout: Arc<AmbientScout>,
    luna_input: Arc<LunaInput>,
    drive_input: Arc<DriveInputStore>,
    mail_input: Arc<MailInputStore>,
    sensing: Arc<SensingStore>,
    signals: Arc<SignalStore>,
    conversation: ConversationCoordinator,
    intents: Arc<ExplorationIntentQueue>,
    manual_runs: Arc<ManualExplorationStore>,
    attempts: Arc<ExplorationAttemptStore>,
    mut trigger_rx: mpsc::Receiver<ExplorationTrigger>,
) {
    let mut config_updates = autonomy.subscribe();
    let started_at = Utc::now();
    let mut last_completed_at = usage
        .latest_exploration_completed_at()
        .await
        .ok()
        .flatten()
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc));
    let last_skipped_at = attempts
        .latest()
        .await
        .and_then(|attempt| DateTime::parse_from_rfc3339(&attempt.at).ok())
        .map(|value| value.with_timezone(&Utc));
    let mut last_attempt_at = match (last_completed_at, last_skipped_at) {
        (Some(completed), Some(skipped)) => Some(completed.max(skipped)),
        (Some(completed), None) => Some(completed),
        (None, Some(skipped)) => Some(skipped),
        (None, None) => None,
    };
    let mut pending_triggers = VecDeque::new();

    loop {
        let config = autonomy.snapshot().await;
        let profile_snapshot = profile.snapshot().await;
        let now = Utc::now();
        let headline = match usage.headline(&today_started_at()).await {
            Ok(headline) => headline,
            Err(error) => {
                set_error(&state, &manual_runs, None, error.to_string()).await;
                sleep(POLICY_REFRESH).await;
                continue;
            }
        };
        let scheduled_at = last_attempt_at
            .map(|last| last + chrono::Duration::minutes(config.interval_minutes as i64))
            .unwrap_or_else(|| {
                started_at + chrono::Duration::minutes(config.interval_minutes as i64)
            });
        // Interactive conversation owns the single Codex transport. Keep every
        // background trigger queued while the user is waiting, rather than only
        // delaying triggers that happened to be queued first.
        let conversation_busy = conversation.snapshot().await.active;
        let effective_scheduled_at = if conversation_busy {
            now + chrono::Duration::seconds(2)
        } else {
            scheduled_at
        };
        let pending_trigger = pending_triggers.front();
        let gate = evaluate_gate(
            &config,
            profile_snapshot.status == SetupStatus::Ready,
            &headline,
            now,
            effective_scheduled_at,
            pending_trigger.is_some() && !conversation_busy,
            pending_trigger.is_some_and(ExplorationTrigger::bypasses_quiet_hours),
            pending_trigger.is_some_and(ExplorationTrigger::bypasses_token_limit),
            pending_trigger.is_none()
                || pending_trigger.is_some_and(ExplorationTrigger::bypasses_message_limit),
        );

        match gate {
            Gate::Run => {
                let mut trigger = pending_triggers.pop_front();
                if let Some(ExplorationTrigger::Intent(intent)) = trigger.as_ref() {
                    match intents.claim(&intent.id).await {
                        Ok(Some(claimed)) => trigger = Some(ExplorationTrigger::Intent(claimed)),
                        Ok(None) => continue,
                        Err(error) => {
                            set_error(&state, &manual_runs, trigger.as_ref(), error.to_string())
                                .await;
                            continue;
                        }
                    }
                }
                let intent_id = trigger
                    .as_ref()
                    .and_then(ExplorationTrigger::intent)
                    .map(|intent| intent.id.clone());
                let run_trigger = trigger.clone();
                let result = run_once(
                    Arc::clone(&state),
                    Arc::clone(&manual_runs),
                    config.clone(),
                    Arc::clone(&codex),
                    Arc::clone(&inference),
                    Arc::clone(&compute),
                    Arc::clone(&profile),
                    Arc::clone(&continuity),
                    Arc::clone(&context),
                    Arc::clone(&curiosity),
                    Arc::clone(&reflection),
                    Arc::clone(&usage),
                    Arc::clone(&ambient_scout),
                    Arc::clone(&luna_input),
                    Arc::clone(&drive_input),
                    Arc::clone(&mail_input),
                    Arc::clone(&sensing),
                    Arc::clone(&signals),
                    conversation.clone(),
                    trigger.clone(),
                    Arc::clone(&attempts),
                )
                .await;
                match result {
                    Ok(completion) => {
                        if completion.status == ExplorationIntentStatus::Superseded
                            && matches!(trigger, Some(ExplorationTrigger::Manual { .. }))
                        {
                            let reason = completion
                                .retry_reason
                                .as_deref()
                                .unwrap_or("background_interrupted");
                            tracing::info!(
                                target: crate::runtime_log::TARGET,
                                event = "manual_exploration_requeued",
                                request_id = run_trigger
                                    .as_ref()
                                    .and_then(ExplorationTrigger::manual_request)
                                    .map(|(id, _)| id)
                                    .unwrap_or("unknown"),
                                reason,
                                "manual exploration yielded to higher-priority work"
                            );
                            pending_triggers
                                .push_front(trigger.expect("manual retry retains its trigger"));
                            sleep(RETRY_DELAY).await;
                            continue;
                        }
                        // Triggered work can be requeued or completed by its owning
                        // intent. A scheduled pass has no such owner, so even when it
                        // yields to higher-priority activity it must consume the
                        // attempt; otherwise the policy refresh immediately starts the
                        // full intake again and can hot-loop on a degraded channel.
                        if should_advance_attempt_watermark(completion.status, trigger.as_ref()) {
                            advance_attempt_watermark(
                                &mut last_attempt_at,
                                completion.attempted_at,
                            );
                            if completion.completed {
                                last_completed_at = Some(completion.attempted_at);
                            }
                        }
                        if let Some(id) = intent_id {
                            let _ = intents
                                .complete(
                                    &id,
                                    completion.status,
                                    completion.trace_id,
                                    completion.result_revision_id,
                                    None,
                                )
                                .await;
                        }
                    }
                    Err(error) => {
                        if crate::codex::is_recoverable_connection_error(&error)
                            && let Some(manual_trigger @ ExplorationTrigger::Manual { .. }) =
                                trigger.clone()
                        {
                            let request_id = manual_trigger
                                .manual_request()
                                .map(|(request_id, _)| request_id)
                                .expect("manual trigger has a request id");
                            if let Err(persistence_error) = manual_runs
                                .requeue(request_id, Some("codex_reconnecting"))
                                .await
                            {
                                set_error(
                                    &state,
                                    &manual_runs,
                                    trigger.as_ref(),
                                    format!(
                                        "{error}; could not preserve the exploration retry: {persistence_error}"
                                    ),
                                )
                                .await;
                                continue;
                            }
                            refresh_manual_projection(&state, &manual_runs).await;
                            {
                                let mut snapshot = state.write().await;
                                snapshot.phase = ExplorationPhase::Waiting;
                                snapshot.current_activity = None;
                                snapshot.current_trigger = None;
                                snapshot.last_error =
                                    Some("Codex 正在重连；这次探索会自动重新开始。".to_owned());
                            }
                            tracing::warn!(
                                target: crate::runtime_log::TARGET,
                                event = "manual_exploration_requeued",
                                request_id,
                                error = %error,
                                "manual exploration will retry after Codex reconnects"
                            );
                            pending_triggers.push_front(manual_trigger);
                            let retry_delay = if error
                                .to_string()
                                .to_ascii_lowercase()
                                .contains("reconnecting")
                            {
                                Duration::from_secs(10)
                            } else {
                                RETRY_DELAY
                            };
                            sleep(retry_delay).await;
                            continue;
                        }
                        if let Some(id) = intent_id {
                            let _ = intents
                                .complete(
                                    &id,
                                    ExplorationIntentStatus::Failed,
                                    None,
                                    None,
                                    Some(error.to_string()),
                                )
                                .await;
                        }
                        // A failed pass still consumed the scheduled attempt. Without
                        // advancing this watermark, the 30-second policy refresh starts
                        // another full intake immediately and can monopolize the shared
                        // Codex transport while an upstream dependency is unhealthy.
                        advance_attempt_watermark(&mut last_attempt_at, Utc::now());
                        tracing::warn!(
                            target: crate::runtime_log::TARGET,
                            event = "exploration_failed_with_backoff",
                            trigger = trigger
                                .as_ref()
                                .map(ExplorationTrigger::as_str)
                                .unwrap_or("scheduled"),
                            interval_minutes = config.interval_minutes,
                            error = %error,
                            "exploration failed; the configured interval will apply before retry"
                        );
                        set_error(&state, &manual_runs, trigger.as_ref(), error.to_string()).await;
                    }
                }
                continue;
            }
            Gate::Wait { phase, until } => {
                update_waiting_state(&state, phase, until, last_completed_at).await;
                let sleep_for = until
                    .map(|until| {
                        (until - now)
                            .to_std()
                            .unwrap_or(Duration::ZERO)
                            .min(POLICY_REFRESH)
                    })
                    .unwrap_or(POLICY_REFRESH);
                tokio::select! {
                    _ = sleep(sleep_for) => {}
                    changed = config_updates.changed() => {
                        if changed.is_err() {
                            break;
                        }
                    }
                    trigger = trigger_rx.recv() => {
                        let Some(trigger) = trigger else {
                            break;
                        };
                        if matches!(trigger, ExplorationTrigger::Manual { .. }) {
                            pending_triggers.push_front(trigger);
                        } else {
                            pending_triggers.push_back(trigger);
                        }
                    }
                }
            }
        }
    }
}

enum Gate {
    Run,
    Wait {
        phase: ExplorationPhase,
        until: Option<DateTime<Utc>>,
    },
}

fn evaluate_gate(
    config: &AutonomyConfig,
    initialized: bool,
    usage: &UsageHeadline,
    now: DateTime<Utc>,
    scheduled_at: DateTime<Utc>,
    force_due: bool,
    bypass_quiet_hours: bool,
    bypass_token_limit: bool,
    bypass_message_limit: bool,
) -> Gate {
    if !config.enabled {
        return Gate::Wait {
            phase: ExplorationPhase::Disabled,
            until: None,
        };
    }
    if !initialized {
        return Gate::Wait {
            phase: ExplorationPhase::NeedsSetup,
            until: None,
        };
    }
    if !bypass_token_limit
        && config.daily_token_limit > 0
        && usage.autonomous_tokens_today >= config.daily_token_limit
    {
        return Gate::Wait {
            phase: ExplorationPhase::TokenLimit,
            until: Some(next_local_day_start(now)),
        };
    }
    if !bypass_message_limit && all_budgets_exhausted(config, usage) {
        return Gate::Wait {
            phase: ExplorationPhase::MessageLimit,
            until: Some(next_local_day_start(now)),
        };
    }
    if !bypass_quiet_hours && let Some(quiet_end) = quiet_end(now, &config.quiet_hours) {
        return Gate::Wait {
            phase: ExplorationPhase::QuietHours,
            until: Some(quiet_end),
        };
    }
    if force_due || now >= scheduled_at {
        Gate::Run
    } else {
        Gate::Wait {
            phase: ExplorationPhase::Waiting,
            until: Some(scheduled_at),
        }
    }
}

fn advance_attempt_watermark(
    last_attempt_at: &mut Option<DateTime<Utc>>,
    attempted_at: DateTime<Utc>,
) {
    *last_attempt_at = Some(
        last_attempt_at
            .map(|last| last.max(attempted_at))
            .unwrap_or(attempted_at),
    );
}

fn should_advance_attempt_watermark(
    status: ExplorationIntentStatus,
    trigger: Option<&ExplorationTrigger>,
) -> bool {
    status != ExplorationIntentStatus::Superseded || trigger.is_none()
}

async fn update_waiting_state(
    state: &RwLock<ExplorationSnapshot>,
    phase: ExplorationPhase,
    until: Option<DateTime<Utc>>,
    last_run_at: Option<DateTime<Utc>>,
) {
    let mut snapshot = state.write().await;
    snapshot.phase = phase;
    snapshot.next_run_at = until.map(timestamp);
    snapshot.last_run_at = last_run_at.map(timestamp);
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    state: Arc<RwLock<ExplorationSnapshot>>,
    manual_runs: Arc<ManualExplorationStore>,
    autonomy_config: AutonomyConfig,
    codex: Arc<Mutex<CodexClient>>,
    inference: Arc<InferenceExecutor>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
    ambient_scout: Arc<AmbientScout>,
    luna_input: Arc<LunaInput>,
    drive_input: Arc<DriveInputStore>,
    mail_input: Arc<MailInputStore>,
    sensing: Arc<SensingStore>,
    signals: Arc<SignalStore>,
    conversation: ConversationCoordinator,
    trigger: Option<ExplorationTrigger>,
    attempts: Arc<ExplorationAttemptStore>,
) -> Result<ExplorationRunCompletion> {
    let completes_deferred_follow_up =
        matches!(&trigger, Some(ExplorationTrigger::DeferredFollowUp));
    let scheduled = trigger.is_none();
    let started_at = timestamp(Utc::now());
    let starting_candidate_count = sensing.count().await.unwrap_or_default();
    let mut manual_retry_started_at = None;
    if let Some((request_id, _)) = trigger
        .as_ref()
        .and_then(ExplorationTrigger::manual_request)
    {
        let run = manual_runs
            .mark_exploring(request_id, started_at.clone())
            .await?;
        manual_retry_started_at = run
            .filter(|run| run.attempts > 1)
            .and_then(|run| DateTime::parse_from_rfc3339(&run.requested_at).ok())
            .map(|requested_at| requested_at.with_timezone(&Utc));
        refresh_manual_projection(&state, &manual_runs).await;
    }
    {
        let mut snapshot = state.write().await;
        snapshot.phase = ExplorationPhase::Exploring;
        snapshot.next_run_at = None;
        snapshot.last_error = None;
        snapshot.pending_candidate_count = starting_candidate_count;
        snapshot.current_review_candidate_count = 0;
        snapshot.last_reviewed_candidate_count = 0;
        snapshot.current_activity = Some(ExplorationActivity {
            label: trigger
                .as_ref()
                .map(ExplorationTrigger::preparation_label)
                .unwrap_or("准备低成本感知")
                .to_owned(),
            model: String::new(),
            display_name: String::new(),
            effort: String::new(),
            lane: if scheduled { "sense" } else { "observe" }.to_owned(),
        });
        snapshot.current_trigger = Some(
            trigger
                .as_ref()
                .map(ExplorationTrigger::as_str)
                .unwrap_or("scheduled")
                .to_owned(),
        );
    }
    if let Some((request_id, _)) = trigger
        .as_ref()
        .and_then(ExplorationTrigger::manual_request)
    {
        tracing::info!(
            target: crate::runtime_log::TARGET,
            event = "manual_exploration_started",
            request_id,
            "manual exploration started"
        );
    }

    let compute = compute.snapshot().await;
    let profile = profile.snapshot().await;
    let recent_messages = continuity.recent_messages(EXPLORATION_CHAT_TAIL).await?;
    let source_history = continuity.recent_messages(SENSING_SOURCE_CHAT_TAIL).await?;
    let deduplication_references =
        sensing_deduplication_references(signals.deduplication_references().await, &source_history);
    let (runtime_tx, mut runtime_rx) = mpsc::channel(64);
    let activity_state = Arc::clone(&state);
    let activity_task = tokio::spawn(async move {
        while let Some(event) = runtime_rx.recv().await {
            let mut snapshot = activity_state.write().await;
            match event {
                RuntimeEvent::Activity {
                    label,
                    model,
                    display_name,
                    effort,
                    lane,
                } => {
                    snapshot.current_activity = Some(ExplorationActivity {
                        label,
                        model,
                        display_name,
                        effort,
                        lane,
                    });
                }
                RuntimeEvent::Reset => {
                    if let Some(activity) = snapshot.current_activity.as_mut() {
                        activity.label = "正在深入探索".to_owned();
                    }
                }
                RuntimeEvent::Delta { .. } => {}
            }
        }
    });

    let Some(input_events) = conversation.subscribe_background_input().await else {
        let pending_candidate_count = sensing.count().await.unwrap_or_default();
        stop_activity_relay(runtime_tx, activity_task).await?;
        settle_sensing_only(
            &state,
            &manual_runs,
            &attempts,
            trigger.as_ref(),
            ExplorationIntentStatus::Superseded,
            "superseded",
            pending_candidate_count,
            Some("conversation_active"),
            false,
        )
        .await?;
        return Ok(ExplorationRunCompletion::superseded("conversation_active"));
    };
    let input_epoch = *input_events.borrow();
    let mut reviewed_candidates = Vec::new();
    let mut deep_candidate_ids = Vec::new();
    let mut published_input_count = 0usize;
    let mut sensing_trace_id = None;
    let drive_configured = drive_input.has_configured_input().await;
    let mailbox_configured = mail_input.has_configured_input().await;
    let runs_intake = trigger_runs_intake(trigger.as_ref());
    if runs_intake {
        let sensing_context = if scheduled {
            let intake_brief = match sensing.next_intake_brief().await {
                Ok(brief) => brief,
                Err(error) => {
                    stop_activity_relay(runtime_tx, activity_task).await?;
                    return Err(error);
                }
            };
            ambient_sensing_context(&recent_messages, &intake_brief, &deduplication_references)
        } else {
            String::new()
        };
        let has_configured_input = ambient_scout.has_configured_input().await
            || drive_input.has_configured_input().await
            || mail_input.has_configured_input().await;
        let document_capacity = sensing.available_capacity().await?;
        let drive = Arc::clone(&drive_input);
        let drive_input_events = input_events.clone();
        let drive_task =
            tokio::spawn(async move { drive.poll(drive_input_events, document_capacity).await });
        let mailbox = Arc::clone(&mail_input);
        let mailbox_input_events = input_events.clone();
        let mailbox_task =
            tokio::spawn(
                async move { mailbox.poll(mailbox_input_events, document_capacity).await },
            );
        let selected_inputs = if scheduled {
            ambient_scout
                .select_inputs(autonomy_config.max_input_parallelism as usize)
                .await
        } else {
            Vec::new()
        };
        let mut luna_config = None;
        let mut external_tasks = JoinSet::new();
        for input in selected_inputs {
            match input {
                AmbientInput::Luna(config) => luna_config = Some(config),
                AmbientInput::External { channel, provider } => {
                    let scout = Arc::clone(&ambient_scout);
                    let context = sensing_context.clone();
                    let input_events = input_events.clone();
                    let events = runtime_tx.clone();
                    external_tasks.spawn(async move {
                        scout
                            .sense_selected(channel, provider, &context, input_events, events)
                            .await
                    });
                }
            }
        }
        let mut sensing_outcomes = Vec::new();
        if let Some(luna_config) = luna_config {
            let Ok(mut client) = codex.try_lock() else {
                external_tasks.abort_all();
                stop_activity_relay(runtime_tx, activity_task).await?;
                settle_sensing_only(
                    &state,
                    &manual_runs,
                    &attempts,
                    trigger.as_ref(),
                    ExplorationIntentStatus::Superseded,
                    "superseded",
                    sensing.count().await.unwrap_or_default(),
                    Some("codex_busy"),
                    false,
                )
                .await?;
                return Ok(ExplorationRunCompletion::superseded("codex_busy"));
            };
            match luna_input
                .sense_selected(
                    luna_config,
                    &mut client,
                    &compute,
                    &profile,
                    &sensing_context,
                    input_events.clone(),
                    runtime_tx.clone(),
                )
                .await
            {
                Ok(outcome) => sensing_outcomes.push(outcome),
                Err(error) => {
                    external_tasks.abort_all();
                    stop_activity_relay(runtime_tx, activity_task).await?;
                    return Err(error);
                }
            }
        }
        while let Some(result) = external_tasks.join_next().await {
            match result.context("join ambient input channel")? {
                Ok(outcome) => sensing_outcomes.push(outcome),
                Err(error) => {
                    stop_activity_relay(runtime_tx, activity_task).await?;
                    return Err(error);
                }
            }
        }
        let drive_outcome = drive_task
            .await
            .context("join Google Drive Inbox polling")??;
        let mailbox_outcome = mailbox_task
            .await
            .context("join research inbox polling")??;
        let mut invocations = sensing_outcomes
            .iter()
            .filter_map(|outcome| outcome.invocation.as_ref())
            .cloned()
            .collect::<Vec<_>>();
        sensing_trace_id = link_sensing_invocations(&mut invocations, None);
        if let Err(error) = usage.record_all(&invocations).await {
            stop_activity_relay(runtime_tx, activity_task).await?;
            return Err(error);
        }
        let interrupted = drive_outcome.interrupted
            || mailbox_outcome.interrupted
            || sensing_outcomes.iter().any(|outcome| outcome.interrupted);
        let channel_failed = invocations.is_empty()
            && (drive_outcome.channel_failure.is_some()
                || mailbox_outcome.inbox_failure.is_some()
                || sensing_outcomes
                    .iter()
                    .any(|outcome| outcome.channel_failure.is_some()));
        if !interrupted {
            let ambient_batches = sensing_outcomes
                .into_iter()
                .filter_map(|outcome| outcome.actor.map(|actor| (outcome.candidates, actor)))
                .collect();
            let batches = prioritize_candidate_batches(
                mailbox_outcome.candidates,
                mailbox_outcome.actor,
                ambient_batches,
            );
            let retained_retry_count = if let Some(requested_at) = manual_retry_started_at {
                sensing
                    .candidates()
                    .await?
                    .iter()
                    .filter(|candidate| {
                        observation_is_current(&candidate.observed_at, requested_at)
                    })
                    .count()
            } else {
                0
            };
            let update = if batches.is_empty() && retained_retry_count > 0 {
                // A recoverable Codex reconnect happens after intake has
                // already advanced the mailbox cursor. Keep the transient
                // candidates created by this same manual request so the next
                // attempt can resume review instead of treating the message
                // as consumed and clearing its evidence.
                Ok(retained_retry_count)
            } else if batches.is_empty() {
                // Unreviewed candidates form a bounded, expiring queue. An
                // empty provider cycle must not erase the remainder of a
                // multi-topic mailbox report after the IMAP cursor advanced.
                sensing.count().await
            } else {
                sensing.enqueue_many(batches).await
            };
            if let Err(error) = update {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
            // Keep the existing mailbox-first admission contract intact.
            // Drive files are immutable, so they can safely wait: represent
            // each complete file atomically in whatever capacity remains,
            // and only then commit its local remote-file cursor.
            if let Some(actor) = drive_outcome.actor {
                let mut acknowledged_file_ids = Vec::new();
                for batch in drive_outcome.batches {
                    if !sensing
                        .enqueue_complete_batch(batch.candidates, actor.clone())
                        .await?
                    {
                        break;
                    }
                    acknowledged_file_ids.push(batch.file_id);
                }
                drive_input.acknowledge_files(acknowledged_file_ids).await?;
            }
        }
        let pending_candidate_count = match sensing.count().await {
            Ok(count) => count,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        if interrupted {
            stop_activity_relay(runtime_tx, activity_task).await?;
            settle_sensing_only(
                &state,
                &manual_runs,
                &attempts,
                trigger.as_ref(),
                ExplorationIntentStatus::Superseded,
                "superseded",
                pending_candidate_count,
                Some("newer_user_input"),
                false,
            )
            .await?;
            return Ok(ExplorationRunCompletion::superseded("newer_user_input"));
        }
        reviewed_candidates = match sensing
            .review_batch(sensing_review_batch_size(scheduled))
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        {
            let mut snapshot = state.write().await;
            snapshot.pending_candidate_count = pending_candidate_count;
            snapshot.current_review_candidate_count = reviewed_candidates.len();
        }
        if scheduled && reviewed_candidates.is_empty() {
            stop_activity_relay(runtime_tx, activity_task).await?;
            let outcome = if channel_failed {
                "channel_failed"
            } else if drive_configured {
                "drive_empty"
            } else if mailbox_configured {
                "mailbox_empty"
            } else if sensing_trace_id.is_none() {
                if has_configured_input {
                    "input_cooldown"
                } else {
                    "no_input_channel"
                }
            } else {
                "no_candidates"
            };
            // A successful mailbox poll is a real observation attempt even
            // when there was no new delivery. Otherwise the activity log
            // would falsely imply that the configured inbox was never run.
            let was_recorded = !invocations.is_empty() || drive_configured || mailbox_configured;
            settle_sensing_only(
                &state,
                &manual_runs,
                &attempts,
                trigger.as_ref(),
                ExplorationIntentStatus::Silent,
                outcome,
                pending_candidate_count,
                None,
                was_recorded,
            )
            .await?;
            return Ok(ExplorationRunCompletion {
                status: ExplorationIntentStatus::Silent,
                trace_id: sensing_trace_id,
                result_revision_id: None,
                retry_reason: None,
                attempted_at: Utc::now(),
                completed: was_recorded,
            });
        }

        if !reviewed_candidates.is_empty() {
            let hard_deduplication =
                hard_deduplicate(&reviewed_candidates, &deduplication_references);
            let mut duplicate_candidate_ids = hard_deduplication.duplicate_candidate_ids;
            let hard_duplicate_count = duplicate_candidate_ids.len();
            let semantic_candidates = hard_deduplication.survivors;
            let (semantic_duplicate_ids, mut dedup_invocations, dedup_interrupted) = match inference
                .classify_sensing_duplicates(
                    &semantic_candidates,
                    &deduplication_references,
                    input_events.clone(),
                )
                .await
            {
                InferenceAttempt::Completed(outcome) => {
                    (outcome.value, outcome.invocations, outcome.interrupted)
                }
                InferenceAttempt::Deferred {
                    reason,
                    invocations,
                } => {
                    tracing::warn!(
                        target: crate::runtime_log::TARGET,
                        event = "sensing_duplicate_classification_deferred",
                        reason,
                        "local duplicate classification failed open; value review will continue"
                    );
                    (
                        Vec::new(),
                        invocations,
                        input_events.has_changed().unwrap_or(true),
                    )
                }
            };
            sensing_trace_id =
                link_sensing_invocations(&mut dedup_invocations, sensing_trace_id.as_deref());
            if let Err(error) = usage.record_all(&dedup_invocations).await {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
            if dedup_interrupted {
                stop_activity_relay(runtime_tx, activity_task).await?;
                settle_sensing_only(
                    &state,
                    &manual_runs,
                    &attempts,
                    trigger.as_ref(),
                    ExplorationIntentStatus::Superseded,
                    "superseded",
                    pending_candidate_count,
                    Some("newer_user_input"),
                    false,
                )
                .await?;
                return Ok(ExplorationRunCompletion::superseded("newer_user_input"));
            }
            duplicate_candidate_ids.extend(semantic_duplicate_ids);
            duplicate_candidate_ids.sort();
            duplicate_candidate_ids.dedup();
            if !duplicate_candidate_ids.is_empty() {
                tracing::info!(
                    target: crate::runtime_log::TARGET,
                    event = "sensing_duplicates_suppressed",
                    hard_duplicate_count,
                    local_model_duplicate_count = duplicate_candidate_ids
                        .len()
                        .saturating_sub(hard_duplicate_count),
                    "suppressed repeated external inputs before value review"
                );
            }
            let value_candidates = semantic_candidates
                .into_iter()
                .filter(|candidate| !duplicate_candidate_ids.contains(&candidate.id))
                .collect::<Vec<_>>();

            let (decisions, mut review_invocations, review_interrupted, review_deferred_reason) =
                if value_candidates.is_empty() {
                    (Vec::new(), Vec::new(), false, None)
                } else {
                    match inference
                        .review_sensing(&value_candidates, input_events.clone())
                        .await
                    {
                        InferenceAttempt::Completed(outcome) => (
                            outcome.value,
                            outcome.invocations,
                            outcome.interrupted,
                            None,
                        ),
                        InferenceAttempt::Deferred {
                            reason,
                            invocations,
                        } => {
                            tracing::warn!(
                                target: crate::runtime_log::TARGET,
                                event = "generic_inference_deferred",
                                task = "ambient_review",
                                reason,
                                "ambient review was deferred for a later intake cycle"
                            );
                            let interrupted = input_events.has_changed().unwrap_or(true);
                            (Vec::new(), invocations, interrupted, Some(reason))
                        }
                    }
                };
            sensing_trace_id =
                link_sensing_invocations(&mut review_invocations, sensing_trace_id.as_deref());
            if let Err(error) = usage.record_all(&review_invocations).await {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
            if review_interrupted {
                stop_activity_relay(runtime_tx, activity_task).await?;
                settle_sensing_only(
                    &state,
                    &manual_runs,
                    &attempts,
                    trigger.as_ref(),
                    ExplorationIntentStatus::Superseded,
                    "superseded",
                    pending_candidate_count,
                    Some("newer_user_input"),
                    false,
                )
                .await?;
                return Ok(ExplorationRunCompletion::superseded("newer_user_input"));
            }

            let mut route_plan = plan_sensing_routes(&value_candidates, decisions);
            route_plan
                .terminal_ids
                .extend(duplicate_candidate_ids.iter().cloned());
            route_plan.terminal_ids.sort();
            route_plan.terminal_ids.dedup();
            let review_metrics = SensingDeliveryMetrics {
                reviewed_candidate_count: reviewed_candidates.len(),
                input_count: route_plan.inputs.len(),
                deep_count: route_plan.deep_candidates.len(),
                discard_count: route_plan
                    .terminal_ids
                    .len()
                    .saturating_sub(route_plan.inputs.len()),
                deferred_candidate_count: route_plan.deferred_ids.len(),
                ..SensingDeliveryMetrics::default()
            };
            let mut suppressed_input_count = 0usize;
            for input in route_plan.inputs {
                match signals
                    .publish_with_presentation(
                        &input.candidate,
                        input.content,
                        input.presentation,
                        input.qualification_note,
                        input.reason,
                    )
                    .await?
                {
                    SignalPublishOutcome::Published => published_input_count += 1,
                    SignalPublishOutcome::Existing | SignalPublishOutcome::RejectedStale => {
                        suppressed_input_count += 1;
                    }
                }
            }
            if published_input_count > 0 {
                let briefing_inputs = signals
                    .briefing_inputs_for_local_day(Local::now().date_naive())
                    .await;
                let topic_language = ambient_scout.luna_output_language().await;
                match inference
                    .classify_briefing_topics(
                        &briefing_inputs,
                        input_events.clone(),
                        topic_language,
                    )
                    .await
                {
                    InferenceAttempt::Completed(outcome) if !outcome.interrupted => {
                        if let Err(error) = signals
                            .settle_briefing_topics_for_local_day(
                                Local::now().date_naive(),
                                &outcome.value,
                            )
                            .await
                        {
                            stop_activity_relay(runtime_tx, activity_task).await?;
                            return Err(error);
                        }
                        if let Err(error) = usage.record_all(&outcome.invocations).await {
                            stop_activity_relay(runtime_tx, activity_task).await?;
                            return Err(error);
                        }
                    }
                    InferenceAttempt::Completed(_) => {
                        signals
                            .settle_briefing_topics_for_local_day(Local::now().date_naive(), &[])
                            .await?;
                        tracing::debug!(
                            target: crate::runtime_log::TARGET,
                            event = "input_briefing_topics_interrupted",
                            "deferred local input briefing labels for newer user input"
                        );
                    }
                    InferenceAttempt::Deferred {
                        reason,
                        invocations,
                    } => {
                        signals
                            .mark_briefing_topics_unavailable_for_local_day(
                                Local::now().date_naive(),
                            )
                            .await?;
                        tracing::debug!(
                            target: crate::runtime_log::TARGET,
                            event = "input_briefing_topics_deferred",
                            %reason,
                            "kept input briefing entries unclassified"
                        );
                        if let Err(error) = usage.record_all(&invocations).await {
                            stop_activity_relay(runtime_tx, activity_task).await?;
                            return Err(error);
                        }
                    }
                }
            }
            if review_deferred_reason.is_none() {
                annotate_sensing_delivery(
                    &mut review_invocations,
                    SensingDeliveryMetrics {
                        published_input_count,
                        suppressed_input_count,
                        ..review_metrics
                    },
                );
                usage.record_all(&review_invocations).await?;
            }
            sensing
                .settle_review(&route_plan.terminal_ids, &route_plan.deferred_ids)
                .await?;
            let remaining_candidate_count = sensing.count().await?;
            {
                let mut snapshot = state.write().await;
                snapshot.pending_candidate_count = remaining_candidate_count;
                snapshot.current_review_candidate_count = 0;
                snapshot.last_reviewed_candidate_count = review_metrics.reviewed_candidate_count;
            }
            reviewed_candidates = route_plan.deep_candidates;
            deep_candidate_ids = reviewed_candidates
                .iter()
                .map(|candidate| candidate.id.clone())
                .collect();
            if should_settle_after_sensing_review(
                reviewed_candidates.is_empty(),
                scheduled,
                published_input_count,
                review_deferred_reason.is_some(),
            ) {
                let pending_candidate_count = sensing.count().await?;
                let review_deferred = review_deferred_reason.is_some();
                let outcome = if review_deferred {
                    "ambient_review_deferred"
                } else if published_input_count > 0 {
                    "input_signals_published"
                } else {
                    "reviewed_silent"
                };
                stop_activity_relay(runtime_tx, activity_task).await?;
                settle_sensing_only(
                    &state,
                    &manual_runs,
                    &attempts,
                    trigger.as_ref(),
                    ExplorationIntentStatus::Silent,
                    outcome,
                    pending_candidate_count,
                    review_deferred.then_some("infer_runtime_unavailable"),
                    true,
                )
                .await?;
                return Ok(ExplorationRunCompletion {
                    status: ExplorationIntentStatus::Silent,
                    trace_id: sensing_trace_id,
                    result_revision_id: None,
                    retry_reason: review_deferred.then(|| "infer_runtime_unavailable".to_owned()),
                    attempted_at: Utc::now(),
                    completed: !review_deferred,
                });
            }
        }
    }
    let recent_explorations = match usage.recent_explorations(EXPLORATION_JOURNAL_RUNS).await {
        Ok(runs) => runs,
        Err(error) => {
            stop_activity_relay(runtime_tx, activity_task).await?;
            return Err(error);
        }
    };
    let continuity_context = match async {
        Ok::<_, anyhow::Error>(format!(
            "{}\n\n{}\n\n{}\n\n{}\n\n{}",
            continuity.context_seed(None).await,
            context.exploration_prompt().await?,
            curiosity.exploration_prompt().await?,
            autonomy_config.attention_context(),
            exploration_working_context(
                &recent_messages,
                &recent_explorations,
                &reviewed_candidates,
                trigger.as_ref(),
                Utc::now(),
            )
        ))
    }
    .await
    {
        Ok(context) => context,
        Err(error) => {
            stop_activity_relay(runtime_tx, activity_task).await?;
            return Err(error);
        }
    };
    if conversation.current_input_epoch() != input_epoch || conversation.snapshot().await.active {
        let pending_candidate_count = sensing.count().await.unwrap_or_default();
        stop_activity_relay(runtime_tx, activity_task).await?;
        settle_sensing_only(
            &state,
            &manual_runs,
            &attempts,
            trigger.as_ref(),
            ExplorationIntentStatus::Superseded,
            "superseded",
            pending_candidate_count,
            Some("newer_user_input"),
            false,
        )
        .await?;
        return Ok(ExplorationRunCompletion::superseded("newer_user_input"));
    }
    let Ok(mut client) = codex.try_lock() else {
        let pending_candidate_count = sensing.count().await.unwrap_or_default();
        sleep(Duration::from_secs(2)).await;
        stop_activity_relay(runtime_tx, activity_task).await?;
        settle_sensing_only(
            &state,
            &manual_runs,
            &attempts,
            trigger.as_ref(),
            ExplorationIntentStatus::Superseded,
            "superseded",
            pending_candidate_count,
            Some("codex_busy"),
            false,
        )
        .await?;
        return Ok(ExplorationRunCompletion::superseded("codex_busy"));
    };
    let outcome = client
        .explore(
            &compute,
            &profile,
            &continuity_context,
            input_events,
            runtime_tx,
        )
        .await;
    drop(client);
    activity_task
        .await
        .context("join exploration activity relay")?;
    let mut outcome = outcome?;
    if let Some(root_trace_id) = sensing_trace_id.as_ref() {
        for invocation in &mut outcome.invocations {
            invocation.parent_id = Some(root_trace_id.clone());
        }
        outcome.metadata.trace_id = Some(root_trace_id.clone());
    }
    let trace_id = outcome.metadata.trace_id.clone().or_else(|| {
        outcome
            .invocations
            .first()
            .map(|invocation| invocation.id.clone())
    });
    let superseded = outcome.superseded || outcome.interrupted;
    let intent_sources = trigger
        .as_ref()
        .and_then(ExplorationTrigger::intent)
        .map(|intent| intent.source_revision_ids.clone())
        .unwrap_or_default();

    let can_publish = !outcome.interrupted
        && conversation.current_input_epoch() == input_epoch
        && !conversation.snapshot().await.active;
    let headline = usage.headline(&today_started_at()).await?;
    let published = if can_publish
        && let Some(outreach) = outcome.outreach.as_ref()
        && crate::outreach::has_budget(outreach.kind, &autonomy_config, &headline)
    {
        let mut input_revision_ids = outcome.context_revision_ids;
        input_revision_ids.extend(outreach.source_revision_ids.clone());
        input_revision_ids.extend(intent_sources);
        let surfaced_hunch_revision_ids = outcome
            .hunch_revisions
            .iter()
            .map(|reference| reference.revision_id.clone())
            .collect::<Vec<_>>();
        input_revision_ids.extend(
            recent_messages
                .iter()
                .filter_map(|entry| entry.revision_id.clone()),
        );
        input_revision_ids.sort();
        input_revision_ids.dedup();
        let stored = continuity
            .ingest_message(
                MemoryRole::Assistant,
                &outreach.message,
                Vec::new(),
                Some(outcome.metadata),
                MessageLinks {
                    responds_to: None,
                    continues_from: None,
                    input_revision_ids,
                    surfaced_hunch_revision_ids,
                    quotes: Vec::new(),
                    topic: None,
                },
            )
            .await?;
        for reference in outcome.hunch_revisions {
            curiosity
                .mark_surfaced(
                    &reference.page_id,
                    &reference.revision_id,
                    &stored.page.revision_id,
                )
                .await?;
        }
        reflection
            .record_message(&stored.entry, None, false, &[])
            .await?;
        Some(stored.entry)
    } else {
        None
    };

    if published.is_some()
        && let Some(invocation) = outcome.invocations.last_mut()
    {
        invocation.produced_message = true;
    }
    usage.record_all(&outcome.invocations).await?;

    if completes_deferred_follow_up && !outcome.interrupted {
        reflection
            .complete_triggered_follow_ups(if published.is_some() {
                "messaged"
            } else {
                "silent"
            })
            .await?;
    }

    let status = if superseded {
        ExplorationIntentStatus::Superseded
    } else if published.is_some() {
        ExplorationIntentStatus::Messaged
    } else {
        ExplorationIntentStatus::Silent
    };
    if !superseded {
        sensing.remove(&deep_candidate_ids).await?;
    }
    let outcome_label = match status {
        ExplorationIntentStatus::Messaged => outcome
            .outreach
            .as_ref()
            .map(|outreach| format!("messaged_{}", outreach.kind.as_str()))
            .unwrap_or_else(|| "messaged".to_owned()),
        ExplorationIntentStatus::Superseded => "superseded".to_owned(),
        _ if published_input_count > 0 => "input_signals_published".to_owned(),
        _ => "silent".to_owned(),
    };
    let completed_at = timestamp(Utc::now());
    let result_revision_id = published
        .as_ref()
        .and_then(|message| message.revision_id.clone());
    let pending_candidate_count = sensing.count().await?;
    let retry_reason = (status == ExplorationIntentStatus::Superseded).then(|| {
        if outcome.interrupted {
            "newer_user_input".to_owned()
        } else {
            "superseded".to_owned()
        }
    });
    if let Some((request_id, _)) = trigger
        .as_ref()
        .and_then(ExplorationTrigger::manual_request)
    {
        if status == ExplorationIntentStatus::Superseded {
            manual_runs
                .requeue(request_id, retry_reason.as_deref())
                .await?;
        } else {
            let manual_status = match status {
                ExplorationIntentStatus::Messaged => ManualExplorationStatus::Messaged,
                ExplorationIntentStatus::Silent => ManualExplorationStatus::Silent,
                _ => ManualExplorationStatus::Failed,
            };
            manual_runs
                .complete(
                    request_id,
                    manual_status,
                    completed_at.clone(),
                    outcome_label.clone(),
                    result_revision_id.clone(),
                    pending_candidate_count,
                )
                .await?;
        }
        refresh_manual_projection(&state, &manual_runs).await;
    }
    let mut snapshot = state.write().await;
    if status != ExplorationIntentStatus::Superseded {
        snapshot.last_run_at = Some(completed_at.clone());
        snapshot.last_outcome = Some(outcome_label.clone());
        snapshot.last_trigger = Some(
            trigger
                .as_ref()
                .map(ExplorationTrigger::as_str)
                .unwrap_or("scheduled")
                .to_owned(),
        );
        snapshot.last_skipped_attempt = None;
    }
    snapshot.last_error = None;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.current_review_candidate_count = 0;
    snapshot.pending_candidate_count = pending_candidate_count;
    if let Some(message) = published {
        snapshot.latest_message = Some(message);
    }
    drop(snapshot);
    if let Some((request_id, _)) = trigger
        .as_ref()
        .and_then(ExplorationTrigger::manual_request)
        && status != ExplorationIntentStatus::Superseded
    {
        tracing::info!(
            target: crate::runtime_log::TARGET,
            event = "manual_exploration_completed",
            request_id,
            outcome = outcome_label,
            "manual exploration completed"
        );
    }
    Ok(ExplorationRunCompletion {
        status,
        trace_id,
        result_revision_id,
        retry_reason,
        attempted_at: Utc::now(),
        completed: true,
    })
}

struct ExplorationRunCompletion {
    status: ExplorationIntentStatus,
    trace_id: Option<String>,
    result_revision_id: Option<String>,
    retry_reason: Option<String>,
    attempted_at: DateTime<Utc>,
    completed: bool,
}

impl ExplorationRunCompletion {
    fn superseded(reason: &str) -> Self {
        Self {
            status: ExplorationIntentStatus::Superseded,
            trace_id: None,
            result_revision_id: None,
            retry_reason: Some(reason.to_owned()),
            attempted_at: Utc::now(),
            completed: false,
        }
    }
}

fn exploration_working_context(
    messages: &[MemoryEntry],
    runs: &[crate::usage::ExplorationRunSummary],
    candidates: &[SensingCandidate],
    trigger: Option<&ExplorationTrigger>,
    now: DateTime<Utc>,
) -> String {
    let mut lines = vec![
        "Autonomous working context. Use this to continue the relationship and avoid thematic repetition; it is bounded operational memory, not a relevance score."
            .to_owned(),
        match trigger {
            Some(ExplorationTrigger::Manual { .. }) => {
                "Wake reason: the user explicitly requested an exploration cycle.".to_owned()
            }
            Some(ExplorationTrigger::DeferredFollowUp) => {
                "Wake reason: a deferred conversational continuation has reached its earliest useful time. Reconsider it against everything said since it was scheduled. Continue only if it is still live and can enter the present conversation naturally; otherwise remain silent."
                    .to_owned()
            }
            Some(ExplorationTrigger::Intent(intent)) => format!(
                "Wake reason: the model explicitly requested an evidence-seeking exploration from recent thought. Re-evaluate it against the latest conversation before searching.\n<exploration-intent id=\"{}\" origin=\"{}\">\nquestion: {}\nwhy-now: {}\nsource-revisions: {}\n</exploration-intent>",
                intent.id,
                intent.origin.as_str(),
                intent.question,
                intent.why_now,
                intent.source_revision_ids.join(", ")
            ),
            None => "Wake reason: scheduled exploration cycle.".to_owned(),
        },
        conversation_edge(messages, now),
        "<recent-conversation>".to_owned(),
    ];
    for entry in messages {
        let role = match entry.role {
            MemoryRole::User => "user",
            MemoryRole::Assistant => "assistant",
            MemoryRole::Memory => "memory",
        };
        let origin = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.origin.as_deref())
            .unwrap_or("conversation");
        let content = bounded_message_excerpt(&entry.content, EXPLORATION_MESSAGE_EXCERPT_CHARS);
        let revision = entry.revision_id.as_deref().unwrap_or("");
        lines.push(format!(
            "<message role=\"{role}\" origin=\"{origin}\" at=\"{}\" revision=\"{revision}\">{content}</message>",
            entry.at
        ));
    }
    lines.push("</recent-conversation>".to_owned());
    lines.push("<recent-exploration-journal>".to_owned());
    for run in runs {
        let message = run.message.as_deref().unwrap_or("[silent]");
        let queries = if run.search_queries.is_empty() {
            "none".to_owned()
        } else {
            run.search_queries.join(" | ")
        };
        let reasoning = run
            .reasoning_summaries
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!(
            "<exploration at=\"{}\" surfaced=\"{}\">\nqueries: {}\ninternal-summary: {}\nmessage: {}\n</exploration>",
            run.completed_at,
            run.surfaced,
            queries,
            reasoning.chars().take(1_200).collect::<String>(),
            message.chars().take(1_200).collect::<String>()
        ));
    }
    lines.push("</recent-exploration-journal>".to_owned());
    if !candidates.is_empty() {
        lines.push(format_candidate_pool(candidates));
    }
    let joined = lines.join("\n");
    if joined.chars().count() <= EXPLORATION_CONTEXT_CHARS {
        return joined;
    }
    let mut truncated = joined
        .chars()
        .take(EXPLORATION_CONTEXT_CHARS)
        .collect::<String>();
    truncated.push_str("\n[older working context truncated]");
    truncated
}

fn sensing_deduplication_references(
    mut references: Vec<SensingDeduplicationReference>,
    messages: &[MemoryEntry],
) -> Vec<SensingDeduplicationReference> {
    references.extend(
        messages
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry.role, MemoryRole::User | MemoryRole::Assistant))
            .filter_map(|(index, entry)| {
                let source_urls = source_urls(&entry.content);
                (!source_urls.is_empty()).then(|| SensingDeduplicationReference {
                    reference_id: entry
                        .revision_id
                        .clone()
                        .unwrap_or_else(|| format!("conversation:{}:{index}", entry.at)),
                    fingerprint: String::new(),
                    actor_name: match entry.role {
                        MemoryRole::User => "user",
                        MemoryRole::Assistant => "symbiont-d",
                        MemoryRole::Memory => "memory",
                    }
                    .to_owned(),
                    title: bounded_message_excerpt(
                        entry
                            .content
                            .lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or("Conversation source"),
                        180,
                    ),
                    excerpt: bounded_message_excerpt(
                        &entry.content,
                        SENSING_REFERENCE_EXCERPT_CHARS,
                    ),
                    source_urls,
                    event_at: None,
                    observed_at: entry.at.clone(),
                })
            }),
    );
    references.sort_by(|left, right| {
        let left = DateTime::parse_from_rfc3339(&left.observed_at).ok();
        let right = DateTime::parse_from_rfc3339(&right.observed_at).ok();
        right.cmp(&left)
    });
    references.truncate(MAX_SENSING_DEDUPLICATION_REFERENCES);
    references
}

fn ambient_sensing_context(
    messages: &[MemoryEntry],
    brief: &SensingIntakeBrief,
    recent_sources: &[SensingDeduplicationReference],
) -> String {
    let mut lines = vec![
        "<ambient-sensing-context>".to_owned(),
        format!(
            "<intake-channel id=\"{}\" label=\"{}\">{}</intake-channel>",
            brief.id, brief.label, brief.brief
        ),
        "<open-discovery>Also allow one credible signal outside this channel when it has factual novelty, changed interpretation, accumulated real-world evidence, or current discussion value. It may be recent rather than same-day and need not be unknown to the user. Breadth emerges across rotated passes; do not turn one pass into a generic news roundup.</open-discovery>".to_owned(),
        "The recent source ledger records sources already delivered or explicitly discussed. Do not submit the same underlying source again under new prose. A genuinely new revision, result, evidence source, or accumulated reaction may be submitted only when the cited URL makes that change explicit.".to_owned(),
        "<recent-source-ledger role=\"negative-delivery-evidence\">".to_owned(),
    ];
    for reference in recent_sources.iter().take(MAX_SENSING_LEDGER_REFERENCES) {
        lines.push(format!(
            "<source>{}</source>",
            reference
                .source_urls
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.extend([
        "</recent-source-ledger>".to_owned(),
        "The recent user edge below is an optional downstream ranking hint only. It must not gate intake, define the search domain, or be turned into memory or durable interests.".to_owned(),
        "<recent-user-edge role=\"ranking-hint\">".to_owned(),
    ]);
    for entry in messages
        .iter()
        .rev()
        .filter(|entry| entry.role == MemoryRole::User)
        .take(SENSING_CHAT_TAIL)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        lines.push(format!(
            "<message role=\"user\" at=\"{}\">{}</message>",
            entry.at,
            bounded_message_excerpt(&entry.content, SENSING_MESSAGE_EXCERPT_CHARS)
        ));
    }
    lines.push("</recent-user-edge>".to_owned());
    lines.push("</ambient-sensing-context>".to_owned());
    lines.join("\n")
}

async fn stop_activity_relay(
    events: mpsc::Sender<RuntimeEvent>,
    activity_task: JoinHandle<()>,
) -> Result<()> {
    drop(events);
    activity_task
        .await
        .context("join exploration activity relay")
}

fn trigger_runs_intake(trigger: Option<&ExplorationTrigger>) -> bool {
    trigger.is_none() || matches!(trigger, Some(ExplorationTrigger::Manual { .. }))
}

fn should_settle_after_sensing_review(
    deep_candidates_empty: bool,
    scheduled: bool,
    published_input_count: usize,
    review_deferred: bool,
) -> bool {
    deep_candidates_empty && (scheduled || published_input_count > 0 || review_deferred)
}

async fn settle_sensing_only(
    state: &RwLock<ExplorationSnapshot>,
    manual_runs: &ManualExplorationStore,
    attempts: &ExplorationAttemptStore,
    trigger: Option<&ExplorationTrigger>,
    status: ExplorationIntentStatus,
    outcome: &str,
    pending_candidate_count: usize,
    reason: Option<&str>,
    was_recorded: bool,
) -> Result<()> {
    let completed_at = timestamp(Utc::now());
    let skipped_attempt = if status != ExplorationIntentStatus::Superseded && !was_recorded {
        Some(
            attempts
                .record(
                    trigger
                        .map(ExplorationTrigger::as_str)
                        .unwrap_or("scheduled"),
                    outcome,
                )
                .await?,
        )
    } else {
        None
    };
    if let Some((request_id, _)) = trigger.and_then(ExplorationTrigger::manual_request) {
        if status == ExplorationIntentStatus::Superseded {
            manual_runs.requeue(request_id, reason).await?;
        } else {
            manual_runs
                .complete(
                    request_id,
                    ManualExplorationStatus::Silent,
                    completed_at.clone(),
                    outcome.to_owned(),
                    None,
                    pending_candidate_count,
                )
                .await?;
            tracing::info!(
                target: crate::runtime_log::TARGET,
                event = "manual_exploration_completed",
                request_id,
                outcome,
                "manual exploration completed without a published message"
            );
        }
        refresh_manual_projection(state, manual_runs).await;
    }
    let mut snapshot = state.write().await;
    if status != ExplorationIntentStatus::Superseded && was_recorded {
        snapshot.last_run_at = Some(completed_at);
        snapshot.last_outcome = Some(outcome.to_owned());
        snapshot.last_trigger = Some(
            trigger
                .map(ExplorationTrigger::as_str)
                .unwrap_or("scheduled")
                .to_owned(),
        );
        snapshot.last_skipped_attempt = None;
    }
    if let Some(skipped_attempt) = skipped_attempt {
        snapshot.last_skipped_attempt = Some(skipped_attempt);
    }
    snapshot.last_error = None;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.current_review_candidate_count = 0;
    snapshot.pending_candidate_count = pending_candidate_count;
    Ok(())
}

fn conversation_edge(messages: &[MemoryEntry], now: DateTime<Utc>) -> String {
    let Some((last_visible_index, last_visible)) = messages.iter().enumerate().next_back() else {
        return "<conversation-edge state=\"empty\" />".to_owned();
    };
    let last_user = messages
        .iter()
        .enumerate()
        .rfind(|(_, entry)| entry.role == MemoryRole::User);
    let last_user_index = last_user.map(|(index, _)| index);
    let last_direct_reply = last_user_index.and_then(|index| {
        messages
            .iter()
            .enumerate()
            .skip(index + 1)
            .rfind(|(_, entry)| {
                entry.role == MemoryRole::Assistant && message_origin(entry) == "interactive"
            })
    });
    let unsolicited_since_last_user = last_user_index
        .map(|index| {
            messages
                .iter()
                .skip(index + 1)
                .filter(|entry| is_unsolicited_assistant(entry))
                .count()
        })
        .unwrap_or(0);
    let seconds_since_last_user = last_user
        .and_then(|(_, entry)| DateTime::parse_from_rfc3339(&entry.at).ok())
        .map(|at| (now - at.with_timezone(&Utc)).num_seconds().max(0));

    let mut lines = vec![format!(
        "<conversation-edge unsolicited-since-last-user=\"{unsolicited_since_last_user}\"{}>",
        seconds_since_last_user
            .map(|seconds| format!(" seconds-since-last-user=\"{seconds}\""))
            .unwrap_or_default()
    )];
    if let Some((_, entry)) = last_user {
        lines.push(edge_message("last-user-message", entry));
    }
    if let Some((reply_index, entry)) = last_direct_reply
        && reply_index != last_visible_index
    {
        lines.push(edge_message("last-direct-reply", entry));
    }
    lines.push(edge_message("last-visible-message", last_visible));
    if unsolicited_since_last_user > 0 {
        lines.push(
            "<attention-state>The user has not spoken since these unsolicited messages. This is pending attention, not negative feedback. Do not repeat their topic or a close variant. A distinct credible signal may still be left as a note when it has its own connection to the user's long-term map, or as a discussion when it independently deserves conversation; do not treat it as an urgent intervention or pretend it continues the unanswered thread.</attention-state>"
                .to_owned(),
        );
    }
    lines.push("</conversation-edge>".to_owned());
    lines.join("\n")
}

fn edge_message(label: &str, entry: &MemoryEntry) -> String {
    let role = match entry.role {
        MemoryRole::User => "user",
        MemoryRole::Assistant => "assistant",
        MemoryRole::Memory => "memory",
    };
    format!(
        "<{label} role=\"{role}\" origin=\"{}\" at=\"{}\">{}</{label}>",
        message_origin(entry),
        entry.at,
        bounded_message_excerpt(&entry.content, EXPLORATION_EDGE_EXCERPT_CHARS)
    )
}

fn message_origin(entry: &MemoryEntry) -> &str {
    entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.origin.as_deref())
        .unwrap_or("conversation")
}

fn is_unsolicited_assistant(entry: &MemoryEntry) -> bool {
    entry.role == MemoryRole::Assistant
        && matches!(
            message_origin(entry),
            "autonomous" | "reflection" | "maintenance"
        )
}

fn bounded_message_excerpt(content: &str, max_chars: usize) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return content.to_owned();
    }
    let head_chars = max_chars.saturating_mul(2) / 5;
    let tail_chars = max_chars.saturating_sub(head_chars);
    let head = chars.iter().take(head_chars).collect::<String>();
    let tail = chars
        .iter()
        .skip(chars.len().saturating_sub(tail_chars))
        .collect::<String>();
    format!("{head}\n[… middle omitted …]\n{tail}")
}

async fn refresh_manual_projection(
    state: &RwLock<ExplorationSnapshot>,
    manual_runs: &ManualExplorationStore,
) {
    let projection = manual_runs.projection().await;
    let mut snapshot = state.write().await;
    snapshot.manual_run = projection.latest;
    snapshot.manual_receipts = projection.unpresented;
}

async fn set_error(
    state: &RwLock<ExplorationSnapshot>,
    manual_runs: &ManualExplorationStore,
    trigger: Option<&ExplorationTrigger>,
    mut error: String,
) {
    if let Some((request_id, _)) = trigger.and_then(ExplorationTrigger::manual_request) {
        if let Err(persistence_error) = manual_runs.fail(request_id, "runtime_error").await {
            error = format!("{error}; could not persist manual receipt: {persistence_error}");
        }
        refresh_manual_projection(state, manual_runs).await;
        tracing::warn!(
            target: crate::runtime_log::TARGET,
            event = "manual_exploration_failed",
            request_id,
            error,
            "manual exploration failed"
        );
    }
    let mut snapshot = state.write().await;
    snapshot.phase = ExplorationPhase::Error;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.current_review_candidate_count = 0;
    snapshot.last_error = Some(error);
}

pub fn today_started_at() -> String {
    timestamp(local_datetime_to_utc(
        Local::now().date_naive().and_time(NaiveTime::MIN),
    ))
}

fn next_local_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let local = now.with_timezone(&Local);
    let date = local
        .date_naive()
        .checked_add_days(Days::new(1))
        .unwrap_or(local.date_naive());
    local_datetime_to_utc(date.and_time(NaiveTime::MIN))
}

pub(crate) fn quiet_end(now: DateTime<Utc>, quiet: &QuietHours) -> Option<DateTime<Utc>> {
    if !quiet.enabled {
        return None;
    }
    let start = NaiveTime::parse_from_str(&quiet.start, "%H:%M").ok()?;
    let end = NaiveTime::parse_from_str(&quiet.end, "%H:%M").ok()?;
    if start == end {
        return None;
    }
    let local = now.with_timezone(&Local);
    let date = local.date_naive();
    let time = local.time();
    let end_date = if start < end {
        if time < start || time >= end {
            return None;
        }
        date
    } else if time >= start {
        date.checked_add_days(Days::new(1)).unwrap_or(date)
    } else if time < end {
        date
    } else {
        return None;
    };
    Some(local_datetime_to_utc(end_date.and_time(end)))
}

fn local_datetime_to_utc(value: NaiveDateTime) -> DateTime<Utc> {
    let local = match Local.from_local_datetime(&value) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(earlier, _) => earlier,
        LocalResult::None => {
            let fallback = value + chrono::Duration::hours(1);
            match Local.from_local_datetime(&fallback) {
                LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => value,
                LocalResult::None => Utc.from_utc_datetime(&value).with_timezone(&Local),
            }
        }
    };
    local.with_timezone(&Utc)
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn observation_is_current(observed_at: &str, requested_at: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(observed_at)
        .map(|observed_at| observed_at.with_timezone(&Utc) >= requested_at)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{DateTime, TimeZone, Utc};

    use super::{
        ExplorationAttemptStore, ExplorationIntent, ExplorationIntentOrigin,
        ExplorationIntentStatus, ExplorationPhase, ExplorationSnapshot, ExplorationTrigger, Gate,
        ManualExplorationStatus, ManualExplorationStore, advance_attempt_watermark,
        ambient_sensing_context, bounded_message_excerpt, conversation_edge, evaluate_gate,
        exploration_working_context, observation_is_current, quiet_end, refresh_manual_projection,
        sensing_deduplication_references, set_error, settle_sensing_only,
        should_advance_attempt_watermark, should_settle_after_sensing_review, today_started_at,
        trigger_runs_intake,
    };
    use crate::{
        autonomy::{AutonomyConfig, QuietHours},
        memory::{MemoryEntry, MemoryRole, MessageMetadata},
        sensing::SensingIntakeBrief,
        usage::UsageHeadline,
    };

    #[test]
    fn daily_boundary_is_an_rfc3339_timestamp() {
        assert!(DateTime::parse_from_rfc3339(&today_started_at()).is_ok());
    }

    #[test]
    fn a_failed_pass_advances_the_scheduler_watermark_without_regression() {
        let first = Utc.with_ymd_and_hms(2026, 8, 29, 5, 0, 0).unwrap();
        let failed_at = Utc.with_ymd_and_hms(2026, 8, 29, 5, 1, 0).unwrap();
        let mut watermark = Some(first);

        advance_attempt_watermark(&mut watermark, failed_at);
        assert_eq!(watermark, Some(failed_at));

        advance_attempt_watermark(&mut watermark, first);
        assert_eq!(watermark, Some(failed_at));
    }

    #[test]
    fn a_superseded_scheduled_pass_consumes_its_attempt() {
        assert!(should_advance_attempt_watermark(
            ExplorationIntentStatus::Superseded,
            None,
        ));

        let triggered = ExplorationTrigger::DeferredFollowUp;
        assert!(!should_advance_attempt_watermark(
            ExplorationIntentStatus::Superseded,
            Some(&triggered),
        ));
    }

    #[test]
    fn retry_only_retains_candidates_created_for_the_same_manual_request() {
        let requested_at = Utc.with_ymd_and_hms(2026, 8, 9, 18, 11, 53).unwrap();
        assert!(observation_is_current(
            "2026-08-09T18:11:55.120Z",
            requested_at
        ));
        assert!(!observation_is_current(
            "2026-08-09T17:41:28.215Z",
            requested_at
        ));
        assert!(!observation_is_current("not-a-time", requested_at));
    }

    #[test]
    fn a_deferred_manual_review_settles_before_an_empty_codex_exploration() {
        assert!(should_settle_after_sensing_review(true, false, 0, true));
        assert!(!should_settle_after_sensing_review(false, false, 0, true));
        assert!(!should_settle_after_sensing_review(true, false, 0, false));
    }

    #[test]
    fn mailbox_intake_never_hijacks_thought_or_deferred_follow_up_triggers() {
        let manual = ExplorationTrigger::Manual {
            request_id: "manual".to_owned(),
            requested_at: "2026-08-10T00:00:00Z".to_owned(),
            bypass_token_limit: false,
        };
        let intent = ExplorationTrigger::Intent(ExplorationIntent {
            id: "intent".to_owned(),
            question: "A focused question".to_owned(),
            why_now: "A focused reason".to_owned(),
            source_revision_ids: vec![],
            origin: ExplorationIntentOrigin::Interactive,
            status: ExplorationIntentStatus::Queued,
            requested_at: "2026-08-10T00:00:00Z".to_owned(),
            not_before: "2026-08-10T00:00:00Z".to_owned(),
            started_at: None,
            completed_at: None,
            trace_id: None,
            result_revision_id: None,
            error: None,
        });

        assert!(trigger_runs_intake(None));
        assert!(trigger_runs_intake(Some(&manual)));
        assert!(!trigger_runs_intake(Some(
            &ExplorationTrigger::DeferredFollowUp
        )));
        assert!(!trigger_runs_intake(Some(&intent)));
    }

    #[test]
    fn disabled_quiet_hours_never_block() {
        let quiet = QuietHours {
            enabled: false,
            start: "23:00".to_owned(),
            end: "08:00".to_owned(),
        };
        assert!(
            quiet_end(
                Utc.with_ymd_and_hms(2026, 1, 1, 16, 0, 0).single().unwrap(),
                &quiet
            )
            .is_none()
        );
    }

    #[test]
    fn only_a_confirmed_manual_run_bypasses_the_daily_token_limit() {
        let mut config = AutonomyConfig::default();
        config.enabled = true;
        config.daily_token_limit = 100;
        config.daily_interrupt_limit = 1;
        config.quiet_hours.enabled = false;
        let usage = UsageHeadline {
            total_tokens: 1_000,
            autonomous_tokens_today: 100,
            autonomous_messages_today: 1,
            autonomous_interventions_today: 1,
            autonomous_notes_today: 0,
            reflection_tokens_today: 0,
        };
        let now = Utc.with_ymd_and_hms(2026, 7, 31, 8, 0, 0).unwrap();
        let scheduled_at = now - chrono::Duration::minutes(1);

        assert!(matches!(
            evaluate_gate(
                &config,
                true,
                &usage,
                now,
                scheduled_at,
                true,
                true,
                false,
                true,
            ),
            Gate::Wait {
                phase: ExplorationPhase::TokenLimit,
                ..
            }
        ));
        assert!(matches!(
            evaluate_gate(
                &config,
                true,
                &usage,
                now,
                scheduled_at,
                true,
                true,
                true,
                true,
            ),
            Gate::Run
        ));
    }

    #[tokio::test]
    async fn a_deferred_manual_run_keeps_the_last_completed_projection_intact() {
        let path = std::env::temp_dir().join(format!(
            "symbiont-exploration-projection-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let manual_runs = ManualExplorationStore::open(path.clone())
            .await
            .expect("open manual exploration store");
        let attempt_path = path.with_file_name(format!(
            "symbiont-exploration-attempt-test-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let attempts = ExplorationAttemptStore::open(attempt_path.clone())
            .await
            .expect("open attempt log");
        manual_runs
            .accept("explore_test".to_owned(), "2026-08-05T15:00:00Z".to_owned())
            .await
            .expect("accept manual exploration");
        let trigger = ExplorationTrigger::Manual {
            request_id: "explore_test".to_owned(),
            requested_at: "2026-08-05T15:00:00Z".to_owned(),
            bypass_token_limit: false,
        };
        let state = tokio::sync::RwLock::new(ExplorationSnapshot {
            last_run_at: Some("2026-08-05T14:00:00Z".to_owned()),
            last_outcome: Some("messaged_discussion".to_owned()),
            last_trigger: Some("scheduled".to_owned()),
            ..ExplorationSnapshot::default()
        });
        refresh_manual_projection(&state, &manual_runs).await;

        settle_sensing_only(
            &state,
            &manual_runs,
            &attempts,
            Some(&trigger),
            ExplorationIntentStatus::Superseded,
            "superseded",
            0,
            Some("codex_busy"),
            false,
        )
        .await
        .expect("defer manual exploration");

        let snapshot = state.read().await;
        assert_eq!(
            snapshot.last_run_at.as_deref(),
            Some("2026-08-05T14:00:00Z")
        );
        assert_eq!(
            snapshot.last_outcome.as_deref(),
            Some("messaged_discussion")
        );
        assert_eq!(snapshot.last_trigger.as_deref(), Some("scheduled"));
        let manual = snapshot.manual_run.as_ref().unwrap();
        assert_eq!(manual.status, ManualExplorationStatus::Queued);
        assert_eq!(manual.reason.as_deref(), Some("codex_busy"));
        assert!(manual.completed_at.is_none());
        drop(snapshot);

        set_error(
            &state,
            &manual_runs,
            Some(&trigger),
            "transport failed".to_owned(),
        )
        .await;
        let snapshot = state.read().await;
        assert_eq!(
            snapshot.last_outcome.as_deref(),
            Some("messaged_discussion")
        );
        let manual = snapshot.manual_run.as_ref().unwrap();
        assert_eq!(manual.status, ManualExplorationStatus::Failed);
        assert_eq!(manual.outcome.as_deref(), Some("failed"));
        assert!(manual.completed_at.is_some());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(attempt_path);
    }

    #[test]
    fn reconnecting_app_server_errors_are_recoverable() {
        let error = anyhow::anyhow!("Reconnecting... 2/5");
        assert!(crate::codex::is_recoverable_connection_error(&error));
    }

    #[tokio::test]
    async fn an_unconfigured_input_pass_never_replaces_the_last_completed_exploration() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let receipt_path =
            std::env::temp_dir().join(format!("symbiont-unconfigured-input-receipt-{nonce}.json"));
        let attempt_path =
            std::env::temp_dir().join(format!("symbiont-unconfigured-input-attempt-{nonce}.json"));
        let manual_runs = ManualExplorationStore::open(receipt_path.clone())
            .await
            .expect("open receipt store");
        let attempts = ExplorationAttemptStore::open(attempt_path.clone())
            .await
            .expect("open attempt log");
        let state = tokio::sync::RwLock::new(ExplorationSnapshot {
            last_run_at: Some("2026-08-05T14:00:00Z".to_owned()),
            last_outcome: Some("silent".to_owned()),
            last_trigger: Some("scheduled".to_owned()),
            ..ExplorationSnapshot::default()
        });

        settle_sensing_only(
            &state,
            &manual_runs,
            &attempts,
            None,
            ExplorationIntentStatus::Silent,
            "no_input_channel",
            0,
            None,
            false,
        )
        .await
        .expect("settle skipped pass");

        let snapshot = state.read().await;
        assert_eq!(
            snapshot.last_run_at.as_deref(),
            Some("2026-08-05T14:00:00Z")
        );
        assert_eq!(snapshot.last_outcome.as_deref(), Some("silent"));
        assert_eq!(
            snapshot
                .last_skipped_attempt
                .as_ref()
                .map(|attempt| attempt.reason.as_str()),
            Some("no_input_channel")
        );
        drop(snapshot);
        assert_eq!(attempts.recent(4).await.len(), 1);
        let _ = std::fs::remove_file(receipt_path);
        let _ = std::fs::remove_file(attempt_path);
    }

    #[test]
    fn conversation_edge_preserves_the_landing_and_unanswered_attention() {
        let long_user_message = format!(
            "opening context {} final user landing",
            "middle ".repeat(180)
        );
        let messages = vec![
            message(
                MemoryRole::User,
                "2026-07-31T15:52:48Z",
                &long_user_message,
                None,
            ),
            message(
                MemoryRole::Assistant,
                "2026-07-31T15:52:56Z",
                "direct reply",
                Some("interactive"),
            ),
            message(
                MemoryRole::Assistant,
                "2026-08-01T02:07:14Z",
                "latest unsolicited thought",
                Some("autonomous"),
            ),
        ];
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 2, 10, 0).unwrap();

        let edge = conversation_edge(&messages, now);

        assert!(edge.contains("unsolicited-since-last-user=\"1\""));
        assert!(edge.contains("seconds-since-last-user=\"37032\""));
        assert!(edge.contains("opening context"));
        assert!(edge.contains("final user landing"));
        assert!(edge.contains("<last-direct-reply"));
        assert!(edge.contains("latest unsolicited thought"));
        assert!(edge.contains("pending attention, not negative feedback"));
    }

    #[test]
    fn bounded_message_excerpt_keeps_both_ends() {
        let content = format!("begin-{}-end", "x".repeat(1_000));
        let excerpt = bounded_message_excerpt(&content, 100);

        assert!(excerpt.starts_with("begin-"));
        assert!(excerpt.ends_with("-end"));
        assert!(excerpt.contains("middle omitted"));
    }

    #[test]
    fn ambient_sensing_starts_from_a_rotating_channel_and_only_uses_user_text_as_hint() {
        let messages = vec![
            message(
                MemoryRole::User,
                "2026-08-01T00:00:00Z",
                "old user topic",
                None,
            ),
            message(
                MemoryRole::Assistant,
                "2026-08-01T00:01:00Z",
                "assistant framing must not steer intake",
                Some("interactive"),
            ),
            message(
                MemoryRole::User,
                "2026-08-01T00:02:00Z",
                "recent user hint one",
                None,
            ),
            message(
                MemoryRole::User,
                "2026-08-01T00:03:00Z",
                "recent user hint two",
                None,
            ),
        ];
        let brief = SensingIntakeBrief {
            id: "culture_and_ideas",
            label: "Culture and ideas",
            brief: "Scan concrete cultural developments without requiring a project connection.",
        };

        let references = sensing_deduplication_references(Vec::new(), &messages);
        let context = ambient_sensing_context(&messages, &brief, &references);

        assert!(context.contains("intake-channel id=\"culture_and_ideas\""));
        assert!(context.contains("must not gate intake"));
        assert!(context.contains("recent user hint one"));
        assert!(context.contains("recent user hint two"));
        assert!(!context.contains("old user topic"));
        assert!(!context.contains("assistant framing"));
    }

    #[test]
    fn ambient_sensing_treats_discussed_sources_as_negative_delivery_evidence() {
        let messages = vec![message(
            MemoryRole::User,
            "2026-08-01T00:03:00Z",
            "We already discussed [The Collaboration Tax](https://arxiv.org/abs/2608.22152).",
            None,
        )];
        let brief = SensingIntakeBrief {
            id: "research",
            label: "Research and methods",
            brief: "Scan recent research.",
        };

        let references = sensing_deduplication_references(Vec::new(), &messages);
        let context = ambient_sensing_context(&messages, &brief, &references);

        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].source_urls,
            vec!["https://arxiv.org/abs/2608.22152"]
        );
        assert!(context.contains("negative-delivery-evidence"));
        assert!(context.contains("https://arxiv.org/abs/2608.22152"));
        assert!(context.contains("Do not submit the same underlying source again"));
    }

    #[test]
    fn thought_trigger_carries_the_exact_question_into_latest_working_context() {
        let trigger = ExplorationTrigger::Intent(ExplorationIntent {
            id: "intent_test".to_owned(),
            question: "Which runtime evidence would change this design?".to_owned(),
            why_now: "The latest exchange exposed a concrete implementation uncertainty."
                .to_owned(),
            source_revision_ids: vec!["rev_source".to_owned()],
            origin: ExplorationIntentOrigin::Interactive,
            status: ExplorationIntentStatus::Exploring,
            requested_at: "2026-08-01T00:00:00Z".to_owned(),
            not_before: "2026-08-01T00:00:12Z".to_owned(),
            started_at: Some("2026-08-01T00:00:12Z".to_owned()),
            completed_at: None,
            trace_id: None,
            result_revision_id: None,
            error: None,
        });

        let context = exploration_working_context(
            &[],
            &[],
            &[],
            Some(&trigger),
            Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 12).unwrap(),
        );

        assert!(context.contains("<exploration-intent id=\"intent_test\" origin=\"interactive\">"));
        assert!(context.contains("Which runtime evidence would change this design?"));
        assert!(context.contains("rev_source"));
        assert!(context.contains("Re-evaluate it against the latest conversation"));
    }

    fn message(role: MemoryRole, at: &str, content: &str, origin: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            role,
            at: at.to_owned(),
            content: content.to_owned(),
            revision_id: None,
            parts: Vec::new(),
            metadata: origin.map(|origin| MessageMetadata {
                runs: Vec::new(),
                total_tokens: 0,
                duration_ms: 0,
                tool_calls: 0,
                pcp_tool_calls: 0,
                trace_id: None,
                origin: Some(origin.to_owned()),
            }),
            delivery_state: None,
        }
    }
}
