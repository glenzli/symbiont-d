use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tokio::{
    sync::{Mutex, RwLock, mpsc},
    time::sleep,
};

use crate::{
    autonomy::{AutonomyConfig, AutonomyStore, QuietHours},
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeStore,
    continuity::{ContinuityHost, MessageLinks},
    curiosity::CuriosityStore,
    memory::{MemoryEntry, MemoryRole},
    profile::{ProfileStore, SetupStatus},
    reflection::ReflectionStore,
    symbiont_context::SymbiontContextStore,
    usage::{UsageHeadline, UsageStore},
};

const POLICY_REFRESH: Duration = Duration::from_secs(30);
const EXPLORATION_CHAT_TAIL: usize = 14;
const EXPLORATION_JOURNAL_RUNS: usize = 8;
const EXPLORATION_CONTEXT_CHARS: usize = 16_000;

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
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ExplorationTrigger {
    Manual,
    ConversationHunch,
}

impl ExplorationTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ConversationHunch => "conversation_hunch",
        }
    }

    fn preparation_label(self) -> &'static str {
        match self {
            Self::Manual => "准备自主探索",
            Self::ConversationHunch => "准备跟进刚才留下的问题",
        }
    }
}

#[derive(Clone)]
pub struct ExplorationHandle {
    state: Arc<RwLock<ExplorationSnapshot>>,
    trigger: mpsc::Sender<ExplorationTrigger>,
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
    ) -> Self {
        let state = Arc::new(RwLock::new(ExplorationSnapshot::default()));
        let (trigger, trigger_rx) = mpsc::channel(4);
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
            trigger_rx,
        ));
        Self { state, trigger }
    }

    pub async fn snapshot(&self) -> ExplorationSnapshot {
        self.state.read().await.clone()
    }

    pub fn trigger(&self) -> bool {
        self.trigger.try_send(ExplorationTrigger::Manual).is_ok()
    }

    pub fn trigger_conversation_hunch(&self) -> bool {
        self.trigger
            .try_send(ExplorationTrigger::ConversationHunch)
            .is_ok()
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
    mut trigger_rx: mpsc::Receiver<ExplorationTrigger>,
) {
    let mut config_updates = autonomy.subscribe();
    let started_at = Utc::now();
    let mut last_run_at = usage
        .latest_completed_at("autonomous")
        .await
        .ok()
        .flatten()
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc));
    let mut pending_trigger = None;

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
        let gate = evaluate_gate(
            &config,
            profile_snapshot.status == SetupStatus::Ready,
            &headline,
            now,
            scheduled_at,
            pending_trigger.is_some(),
        );

        match gate {
            Gate::Run => {
                let trigger = pending_trigger.take();
                if let Err(error) = run_once(
                    Arc::clone(&state),
                    Arc::clone(&codex),
                    Arc::clone(&compute),
                    Arc::clone(&profile),
                    Arc::clone(&continuity),
                    Arc::clone(&context),
                    Arc::clone(&curiosity),
                    Arc::clone(&reflection),
                    Arc::clone(&usage),
                    trigger,
                )
                .await
                {
                    set_error(&state, error.to_string()).await;
                }
                last_run_at = Some(Utc::now());
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
                        pending_trigger = Some(trigger);
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
    if config.daily_token_limit > 0 && usage.autonomous_tokens_today >= config.daily_token_limit {
        return Gate::Wait {
            phase: ExplorationPhase::TokenLimit,
            until: Some(next_local_day_start(now)),
        };
    }
    if usage.autonomous_messages_today >= config.daily_interrupt_limit as u64 {
        return Gate::Wait {
            phase: ExplorationPhase::MessageLimit,
            until: Some(next_local_day_start(now)),
        };
    }
    if !force_due {
        if let Some(quiet_end) = quiet_end(now, &config.quiet_hours) {
            return Gate::Wait {
                phase: ExplorationPhase::QuietHours,
                until: Some(quiet_end),
            };
        }
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
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
    trigger: Option<ExplorationTrigger>,
) -> Result<()> {
    {
        let mut snapshot = state.write().await;
        snapshot.phase = ExplorationPhase::Exploring;
        snapshot.next_run_at = None;
        snapshot.last_error = None;
        snapshot.current_activity = Some(ExplorationActivity {
            label: trigger
                .map(ExplorationTrigger::preparation_label)
                .unwrap_or("准备定时探索")
                .to_owned(),
            model: String::new(),
            display_name: String::new(),
            effort: String::new(),
            lane: "observe".to_owned(),
        });
        snapshot.current_trigger = Some(
            trigger
                .map(ExplorationTrigger::as_str)
                .unwrap_or("scheduled")
                .to_owned(),
        );
    }

    let compute = compute.snapshot().await;
    let profile = profile.snapshot().await;
    let recent_messages = continuity.recent_messages(EXPLORATION_CHAT_TAIL).await?;
    let recent_explorations = usage.recent_explorations(EXPLORATION_JOURNAL_RUNS).await?;
    let continuity_context = format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}",
        continuity.context_seed(None).await,
        context.prompt().await?,
        curiosity.prompt().await?,
        reflection.prompt().await?,
        exploration_working_context(&recent_messages, &recent_explorations, trigger)
    );
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

    let outcome = codex
        .lock()
        .await
        .explore(&compute, &profile, &continuity_context, runtime_tx)
        .await;
    activity_task
        .await
        .context("join exploration activity relay")?;
    let outcome = outcome?;
    usage.record_all(&outcome.invocations).await?;

    let published = if let Some(text) = outcome.message {
        let mut input_revision_ids = outcome.context_revision_ids;
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
                &text,
                Vec::new(),
                Some(outcome.metadata),
                MessageLinks {
                    responds_to: None,
                    input_revision_ids,
                },
            )
            .await?;
        reflection
            .record_message(&stored.entry, None, false)
            .await?;
        Some(stored.entry)
    } else {
        None
    };

    reflection
        .complete_triggered_follow_ups(if published.is_some() {
            "messaged"
        } else {
            "silent"
        })
        .await?;

    let mut snapshot = state.write().await;
    snapshot.last_run_at = Some(timestamp(Utc::now()));
    snapshot.last_outcome = Some(if published.is_some() {
        "messaged".to_owned()
    } else {
        "silent".to_owned()
    });
    snapshot.last_error = None;
    snapshot.current_activity = None;
    snapshot.current_trigger = None;
    snapshot.last_trigger = Some(
        trigger
            .map(ExplorationTrigger::as_str)
            .unwrap_or("scheduled")
            .to_owned(),
    );
    if let Some(message) = published {
        snapshot.latest_message = Some(message);
    }
    Ok(())
}

fn exploration_working_context(
    messages: &[MemoryEntry],
    runs: &[crate::usage::ExplorationRunSummary],
    trigger: Option<ExplorationTrigger>,
) -> String {
    let mut lines = vec![
        "Autonomous working context. Use this to continue the relationship and avoid thematic repetition; it is bounded operational memory, not a relevance score."
            .to_owned(),
        match trigger {
            Some(ExplorationTrigger::ConversationHunch) => {
                "Wake reason: the preceding conversation created or revised a Hunch. Inspect the freshest active Hunch and decide whether following it now can produce new evidence or a better question. This is an opportunity, not a requirement to search or message."
                    .to_owned()
            }
            Some(ExplorationTrigger::Manual) => {
                "Wake reason: the user explicitly requested an exploration cycle.".to_owned()
            }
            None => "Wake reason: scheduled exploration cycle.".to_owned(),
        },
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
        let content = entry.content.chars().take(900).collect::<String>();
        lines.push(format!(
            "<message role=\"{role}\" origin=\"{origin}\">{content}</message>"
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

fn quiet_end(now: DateTime<Utc>, quiet: &QuietHours) -> Option<DateTime<Utc>> {
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

    use super::{quiet_end, today_started_at};
    use crate::autonomy::QuietHours;

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
}
