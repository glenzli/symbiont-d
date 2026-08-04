mod intent;

pub use intent::{
    ExplorationIntent, ExplorationIntentOrigin, ExplorationIntentQueue, ExplorationIntentReceiver,
    ExplorationIntentStatus, NewExplorationIntent,
};

use std::{collections::VecDeque, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tokio::{
    sync::{Mutex, RwLock, mpsc},
    task::JoinHandle,
    time::sleep,
};

use crate::{
    autonomy::{AutonomyConfig, AutonomyStore, QuietHours},
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeStore,
    continuity::{ContinuityHost, MessageLinks},
    conversation::ConversationCoordinator,
    curiosity::CuriosityStore,
    memory::{MemoryEntry, MemoryRole},
    outreach::all_budgets_exhausted,
    profile::{ProfileStore, SetupStatus},
    reflection::ReflectionStore,
    sensing::{
        REVIEW_BATCH_SIZE, SensingCandidate, SensingIntakeBrief, SensingStore,
        format_candidate_pool,
    },
    symbiont_context::SymbiontContextStore,
    usage::{UsageHeadline, UsageStore},
};

const POLICY_REFRESH: Duration = Duration::from_secs(30);
const EXPLORATION_CHAT_TAIL: usize = 14;
const EXPLORATION_JOURNAL_RUNS: usize = 8;
const EXPLORATION_CONTEXT_CHARS: usize = 16_000;
const EXPLORATION_MESSAGE_EXCERPT_CHARS: usize = 700;
const EXPLORATION_EDGE_EXCERPT_CHARS: usize = 900;
const SENSING_CHAT_TAIL: usize = 2;
const SENSING_MESSAGE_EXCERPT_CHARS: usize = 320;

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
    pub pending_candidate_count: usize,
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
            pending_candidate_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
enum ExplorationTrigger {
    Manual { bypass_token_limit: bool },
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
            Self::Manual { .. } => "准备自主探索",
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
                bypass_token_limit: true
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
}

#[derive(Clone)]
pub struct ExplorationHandle {
    state: Arc<RwLock<ExplorationSnapshot>>,
    trigger: mpsc::Sender<ExplorationTrigger>,
    intents: Arc<ExplorationIntentQueue>,
    sensing: Arc<SensingStore>,
}

impl ExplorationHandle {
    pub fn start(
        autonomy: Arc<AutonomyStore>,
        profile: Arc<ProfileStore>,
        codex: Arc<Mutex<CodexClient>>,
        compute: Arc<ComputeStore>,
        continuity: Arc<ContinuityHost>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        usage: Arc<UsageStore>,
        sensing: Arc<SensingStore>,
        conversation: ConversationCoordinator,
        intents: Arc<ExplorationIntentQueue>,
        mut intent_receiver: ExplorationIntentReceiver,
    ) -> Self {
        let state = Arc::new(RwLock::new(ExplorationSnapshot::default()));
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
            compute,
            continuity,
            context,
            curiosity,
            reflection,
            usage,
            Arc::clone(&sensing),
            conversation,
            Arc::clone(&intents),
            trigger_rx,
        ));
        Self {
            state,
            trigger,
            intents,
            sensing,
        }
    }

    pub async fn snapshot(&self) -> ExplorationSnapshot {
        self.state.read().await.clone()
    }

    pub async fn candidates(&self) -> Result<Vec<SensingCandidate>> {
        self.sensing.candidates().await
    }

    pub fn trigger(&self, bypass_token_limit: bool) -> bool {
        self.trigger
            .try_send(ExplorationTrigger::Manual { bypass_token_limit })
            .is_ok()
    }

    pub fn trigger_follow_up(&self) -> bool {
        self.trigger
            .try_send(ExplorationTrigger::DeferredFollowUp)
            .is_ok()
    }

    pub async fn recent_intents(&self, limit: usize) -> Vec<ExplorationIntent> {
        self.intents.recent(limit).await
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
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
    sensing: Arc<SensingStore>,
    conversation: ConversationCoordinator,
    intents: Arc<ExplorationIntentQueue>,
    mut trigger_rx: mpsc::Receiver<ExplorationTrigger>,
) {
    let mut config_updates = autonomy.subscribe();
    let started_at = Utc::now();
    let mut last_run_at = usage
        .latest_exploration_completed_at()
        .await
        .ok()
        .flatten()
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc));
    let mut pending_triggers = VecDeque::new();

    loop {
        let config = autonomy.snapshot().await;
        let profile_snapshot = profile.snapshot().await;
        let now = Utc::now();
        let headline = match usage.headline(&today_started_at()).await {
            Ok(headline) => headline,
            Err(error) => {
                set_error(&state, error.to_string()).await;
                sleep(POLICY_REFRESH).await;
                continue;
            }
        };
        let scheduled_at = last_run_at
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
                            set_error(&state, error.to_string()).await;
                            continue;
                        }
                    }
                }
                let intent_id = trigger
                    .as_ref()
                    .and_then(ExplorationTrigger::intent)
                    .map(|intent| intent.id.clone());
                let result = run_once(
                    Arc::clone(&state),
                    config.clone(),
                    Arc::clone(&codex),
                    Arc::clone(&compute),
                    Arc::clone(&profile),
                    Arc::clone(&continuity),
                    Arc::clone(&context),
                    Arc::clone(&curiosity),
                    Arc::clone(&reflection),
                    Arc::clone(&usage),
                    Arc::clone(&sensing),
                    conversation.clone(),
                    trigger,
                )
                .await;
                match result {
                    Ok(completion) => {
                        if completion.status != ExplorationIntentStatus::Superseded {
                            last_run_at = Some(Utc::now());
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
                        set_error(&state, error.to_string()).await;
                    }
                }
                continue;
            }
            Gate::Wait { phase, until } => {
                update_waiting_state(&state, phase, until, last_run_at).await;
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
    autonomy_config: AutonomyConfig,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
    sensing: Arc<SensingStore>,
    conversation: ConversationCoordinator,
    trigger: Option<ExplorationTrigger>,
) -> Result<ExplorationRunCompletion> {
    let completes_deferred_follow_up =
        matches!(&trigger, Some(ExplorationTrigger::DeferredFollowUp));
    let scheduled = trigger.is_none();
    {
        let mut snapshot = state.write().await;
        snapshot.phase = ExplorationPhase::Exploring;
        snapshot.next_run_at = None;
        snapshot.last_error = None;
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

    let compute = compute.snapshot().await;
    let profile = profile.snapshot().await;
    let recent_messages = continuity.recent_messages(EXPLORATION_CHAT_TAIL).await?;
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
            ExplorationIntentStatus::Superseded,
            "superseded",
            pending_candidate_count,
        )
        .await;
        return Ok(ExplorationRunCompletion {
            status: ExplorationIntentStatus::Superseded,
            trace_id: None,
            result_revision_id: None,
        });
    };
    let input_epoch = *input_events.borrow();
    let mut reviewed_candidates = Vec::new();
    if scheduled {
        let intake_brief = match sensing.next_intake_brief().await {
            Ok(brief) => brief,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        let Ok(mut client) = codex.try_lock() else {
            let pending_candidate_count = sensing.count().await.unwrap_or_default();
            sleep(Duration::from_secs(2)).await;
            stop_activity_relay(runtime_tx, activity_task).await?;
            settle_sensing_only(
                &state,
                ExplorationIntentStatus::Superseded,
                "superseded",
                pending_candidate_count,
            )
            .await;
            return Ok(ExplorationRunCompletion {
                status: ExplorationIntentStatus::Superseded,
                trace_id: None,
                result_revision_id: None,
            });
        };
        let sensing_outcome = client
            .sense(
                &compute,
                &profile,
                &ambient_sensing_context(&recent_messages, &intake_brief),
                input_events.clone(),
                runtime_tx.clone(),
            )
            .await;
        drop(client);
        let sensing_outcome = match sensing_outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        let trace_id = sensing_outcome
            .invocations
            .first()
            .map(|invocation| invocation.id.clone());
        if let Err(error) = usage.record_all(&sensing_outcome.invocations).await {
            stop_activity_relay(runtime_tx, activity_task).await?;
            return Err(error);
        }
        if !sensing_outcome.interrupted {
            if let Err(error) = sensing.replace(sensing_outcome.candidates).await {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        }
        let pending_candidate_count = match sensing.count().await {
            Ok(count) => count,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        if sensing_outcome.interrupted {
            stop_activity_relay(runtime_tx, activity_task).await?;
            settle_sensing_only(
                &state,
                ExplorationIntentStatus::Superseded,
                "superseded",
                pending_candidate_count,
            )
            .await;
            return Ok(ExplorationRunCompletion {
                status: ExplorationIntentStatus::Superseded,
                trace_id: None,
                result_revision_id: None,
            });
        }
        let headline = match usage.headline(&today_started_at()).await {
            Ok(headline) => headline,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        if all_budgets_exhausted(&autonomy_config, &headline) {
            stop_activity_relay(runtime_tx, activity_task).await?;
            settle_sensing_only(
                &state,
                ExplorationIntentStatus::Silent,
                if pending_candidate_count == 0 {
                    "no_candidates"
                } else {
                    "candidates_waiting"
                },
                pending_candidate_count,
            )
            .await;
            return Ok(ExplorationRunCompletion {
                status: ExplorationIntentStatus::Silent,
                trace_id,
                result_revision_id: None,
            });
        }
        reviewed_candidates = match sensing.review_batch(REVIEW_BATCH_SIZE).await {
            Ok(candidates) => candidates,
            Err(error) => {
                stop_activity_relay(runtime_tx, activity_task).await?;
                return Err(error);
            }
        };
        if reviewed_candidates.is_empty() {
            stop_activity_relay(runtime_tx, activity_task).await?;
            settle_sensing_only(
                &state,
                ExplorationIntentStatus::Silent,
                "no_candidates",
                pending_candidate_count,
            )
            .await;
            return Ok(ExplorationRunCompletion {
                status: ExplorationIntentStatus::Silent,
                trace_id,
                result_revision_id: None,
            });
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
            ExplorationIntentStatus::Superseded,
            "superseded",
            pending_candidate_count,
        )
        .await;
        return Ok(ExplorationRunCompletion {
            status: ExplorationIntentStatus::Superseded,
            trace_id: None,
            result_revision_id: None,
        });
    }
    let Ok(mut client) = codex.try_lock() else {
        let pending_candidate_count = sensing.count().await.unwrap_or_default();
        sleep(Duration::from_secs(2)).await;
        stop_activity_relay(runtime_tx, activity_task).await?;
        settle_sensing_only(
            &state,
            ExplorationIntentStatus::Superseded,
            "superseded",
            pending_candidate_count,
        )
        .await;
        return Ok(ExplorationRunCompletion {
            status: ExplorationIntentStatus::Superseded,
            trace_id: None,
            result_revision_id: None,
        });
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

    let mut snapshot = state.write().await;
    snapshot.last_run_at = Some(timestamp(Utc::now()));
    let status = if superseded {
        ExplorationIntentStatus::Superseded
    } else if published.is_some() {
        ExplorationIntentStatus::Messaged
    } else {
        ExplorationIntentStatus::Silent
    };
    snapshot.last_outcome = Some(match status {
        ExplorationIntentStatus::Messaged => outcome
            .outreach
            .as_ref()
            .map(|outreach| format!("messaged_{}", outreach.kind.as_str()))
            .unwrap_or_else(|| "messaged".to_owned()),
        ExplorationIntentStatus::Superseded => "superseded".to_owned(),
        _ => "silent".to_owned(),
    });
    snapshot.last_error = None;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.last_trigger = Some(
        trigger
            .as_ref()
            .map(ExplorationTrigger::as_str)
            .unwrap_or("scheduled")
            .to_owned(),
    );
    snapshot.pending_candidate_count = sensing.count().await?;
    let result_revision_id = published
        .as_ref()
        .and_then(|message| message.revision_id.clone());
    if let Some(message) = published {
        snapshot.latest_message = Some(message);
    }
    Ok(ExplorationRunCompletion {
        status,
        trace_id,
        result_revision_id,
    })
}

struct ExplorationRunCompletion {
    status: ExplorationIntentStatus,
    trace_id: Option<String>,
    result_revision_id: Option<String>,
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

fn ambient_sensing_context(messages: &[MemoryEntry], brief: &SensingIntakeBrief) -> String {
    let mut lines = vec![
        "<ambient-sensing-context>".to_owned(),
        format!(
            "<intake-channel id=\"{}\" label=\"{}\">{}</intake-channel>",
            brief.id, brief.label, brief.brief
        ),
        "<open-discovery>Also allow one credible, high-information signal outside this channel when it is unusually novel or consequential. Breadth emerges across rotated passes; do not turn one pass into a generic news roundup.</open-discovery>".to_owned(),
        "The recent user edge below is an optional downstream ranking hint only. It must not gate intake, define the search domain, or be turned into memory or durable interests.".to_owned(),
        "<recent-user-edge role=\"ranking-hint\">".to_owned(),
    ];
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

async fn settle_sensing_only(
    state: &RwLock<ExplorationSnapshot>,
    status: ExplorationIntentStatus,
    outcome: &str,
    pending_candidate_count: usize,
) {
    let mut snapshot = state.write().await;
    snapshot.last_run_at = if status == ExplorationIntentStatus::Superseded {
        None
    } else {
        Some(timestamp(Utc::now()))
    };
    snapshot.last_outcome = Some(outcome.to_owned());
    snapshot.last_error = None;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.last_trigger = Some("scheduled".to_owned());
    snapshot.pending_candidate_count = pending_candidate_count;
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
            "<attention-state>The user has not spoken since these unsolicited messages. This is pending attention, not negative feedback. Do not repeat their topic or a close variant. A distinct, credible fresh signal may still be left as a note when it has its own connection to the user's long-term map; do not treat it as an urgent intervention or pretend it continues the unanswered thread.</attention-state>"
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

async fn set_error(state: &RwLock<ExplorationSnapshot>, error: String) {
    let mut snapshot = state.write().await;
    snapshot.phase = ExplorationPhase::Error;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.last_outcome = Some("error".to_owned());
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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};

    use super::{
        ExplorationIntent, ExplorationIntentOrigin, ExplorationIntentStatus, ExplorationPhase,
        ExplorationTrigger, Gate, ambient_sensing_context, bounded_message_excerpt,
        conversation_edge, evaluate_gate, exploration_working_context, quiet_end, today_started_at,
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

        let context = ambient_sensing_context(&messages, &brief);

        assert!(context.contains("intake-channel id=\"culture_and_ideas\""));
        assert!(context.contains("must not gate intake"));
        assert!(context.contains("recent user hint one"));
        assert!(context.contains("recent user hint two"));
        assert!(!context.contains("old user topic"));
        assert!(!context.contains("assistant framing"));
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
