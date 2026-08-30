//! Adversarial review for already-published transient external inputs.
//!
//! This worker never gates or removes input signals. It periodically inspects a
//! small new batch and may publish one evidence-backed challenge into the same
//! non-PCP timeline. Silence is the normal outcome.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{Mutex, RwLock, mpsc},
    time::{MissedTickBehavior, interval},
};

use crate::{
    autonomy::AutonomyStore,
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeStore,
    continuity::ContinuityHost,
    conversation::ConversationCoordinator,
    memory::{MemoryEntry, MemoryRole},
    profile::{ProfileStore, SetupStatus},
    sensing::{InputRoleSnapshot, SensingSource},
    signals::{SignalEvent, SignalPublishOutcome, SignalStore},
    source_identity::stable_source_identities,
    usage::{InvocationRecord, UsageStore},
};

pub const SUBMIT_ATTACKER_ASSESSMENT_TOOL: &str = "submit_attacker_assessment";
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const MIN_BATCH_SIZE: usize = 2;
const MAX_BATCH_SIZE: usize = 6;
const MAX_WAIT_MINUTES: i64 = 15;
/// A challenge needs to arrive beside its source rather than unexpectedly
/// resurfacing an old card after the conversation has moved on.
const MAX_INTERVENING_CONVERSATION_MESSAGES: usize = 4;
const MAX_TRACKED_IDS: usize = 240;
const ISSUE_COOLDOWN_DAYS: i64 = 7;

#[derive(Clone, Debug, Deserialize)]
pub struct AttackerAssessment {
    pub disposition: AttackerDisposition,
    pub issue_key: String,
    pub message: String,
    pub reason: String,
    pub related_signal_ids: Vec<String>,
    #[serde(default)]
    pub sources: Vec<SensingSource>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AttackerDisposition {
    Silent,
    Challenge,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttackerDocument {
    #[serde(default)]
    initialized: bool,
    #[serde(default)]
    reviewed_signal_ids: Vec<String>,
    #[serde(default)]
    published_issues: Vec<PublishedIssue>,
    #[serde(default)]
    last_reviewed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishedIssue {
    key: String,
    published_at: String,
}

struct PendingSignals {
    review: Vec<SignalEvent>,
    skipped: Vec<SignalEvent>,
}

struct AttackerStore {
    path: PathBuf,
    document: RwLock<AttackerDocument>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttackerSnapshot {
    pub phase: &'static str,
    pub pending_signals: usize,
    pub current_batch_size: usize,
    pub last_reviewed_at: Option<String>,
    pub last_published_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct AttackerHandle {
    runtime: Arc<RwLock<AttackerSnapshot>>,
}

impl AttackerHandle {
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        path: PathBuf,
        autonomy: Arc<AutonomyStore>,
        profile: Arc<ProfileStore>,
        codex: Arc<Mutex<CodexClient>>,
        compute: Arc<ComputeStore>,
        signals: Arc<SignalStore>,
        usage: Arc<UsageStore>,
        continuity: Arc<ContinuityHost>,
        conversation: ConversationCoordinator,
    ) -> Result<Self> {
        let store = Arc::new(AttackerStore::open(path).await?);
        let runtime = Arc::new(RwLock::new(AttackerSnapshot::default()));
        tokio::spawn(run(
            Arc::clone(&store),
            Arc::clone(&runtime),
            autonomy,
            profile,
            codex,
            compute,
            signals,
            usage,
            continuity,
            conversation,
        ));
        Ok(Self { runtime })
    }

    pub async fn snapshot(&self) -> AttackerSnapshot {
        self.runtime.read().await.clone()
    }
}

impl AttackerStore {
    async fn open(path: PathBuf) -> Result<Self> {
        let document = match fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content)
                .with_context(|| format!("parse attacker state {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                AttackerDocument::default()
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read attacker state {}", path.display()));
            }
        };
        Ok(Self {
            path,
            document: RwLock::new(document),
        })
    }

    async fn initialize_existing(&self, signals: &[SignalEvent]) -> Result<bool> {
        let mut document = self.document.write().await;
        if document.initialized {
            return Ok(false);
        }
        document.initialized = true;
        document.reviewed_signal_ids = signals.iter().map(|signal| signal.id.clone()).collect();
        trim_front(&mut document.reviewed_signal_ids, MAX_TRACKED_IDS);
        drop(document);
        self.persist().await?;
        Ok(true)
    }

    async fn pending(
        &self,
        signals: &[SignalEvent],
        conversation: &[MemoryEntry],
    ) -> PendingSignals {
        let document = self.document.read().await;
        let reviewed = document
            .reviewed_signal_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut seen_source_identities = signals
            .iter()
            .filter(|signal| reviewed.contains(signal.id.as_str()))
            .flat_map(|signal| {
                stable_source_identities(signal.sources.iter().map(|source| source.url.as_str()))
            })
            .collect::<HashSet<_>>();
        let mut review = Vec::new();
        let mut skipped = Vec::new();
        for signal in signals
            .iter()
            .filter(|signal| !reviewed.contains(signal.id.as_str()))
        {
            let source_identities =
                stable_source_identities(signal.sources.iter().map(|source| source.url.as_str()));
            if source_identities
                .iter()
                .any(|identity| seen_source_identities.contains(identity))
            {
                skipped.push(signal.clone());
                continue;
            }
            seen_source_identities.extend(source_identities);
            if is_near_current_conversation(signal, conversation) {
                if review.len() < MAX_BATCH_SIZE {
                    review.push(signal.clone());
                }
            } else {
                skipped.push(signal.clone());
            }
        }
        PendingSignals { review, skipped }
    }

    async fn complete(&self, batch: &[SignalEvent], issue_key: Option<&str>) -> Result<()> {
        let mut document = self.document.write().await;
        document
            .reviewed_signal_ids
            .extend(batch.iter().map(|signal| signal.id.clone()));
        document.reviewed_signal_ids.sort();
        document.reviewed_signal_ids.dedup();
        trim_front(&mut document.reviewed_signal_ids, MAX_TRACKED_IDS);
        if let Some(issue_key) = issue_key {
            let now = Utc::now();
            document
                .published_issues
                .retain(|issue| issue.key != issue_key && issue_is_in_cooldown(issue, now));
            document.published_issues.push(PublishedIssue {
                key: issue_key.to_owned(),
                published_at: timestamp(now),
            });
            trim_front(&mut document.published_issues, MAX_TRACKED_IDS);
        }
        document.last_reviewed_at = Some(timestamp(Utc::now()));
        drop(document);
        self.persist().await
    }

    async fn issue_was_published(&self, issue_key: &str) -> bool {
        let now = Utc::now();
        self.document
            .read()
            .await
            .published_issues
            .iter()
            .any(|issue| issue.key == issue_key && issue_is_in_cooldown(issue, now))
    }

    async fn persist(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&*self.document.read().await)
            .context("encode attacker state")?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content).await?;
        fs::rename(&temporary, &self.path).await?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    store: Arc<AttackerStore>,
    runtime: Arc<RwLock<AttackerSnapshot>>,
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    signals: Arc<SignalStore>,
    usage: Arc<UsageStore>,
    continuity: Arc<ContinuityHost>,
    conversation: ConversationCoordinator,
) {
    let mut changes = signals.subscribe();
    let mut periodic = interval(CHECK_INTERVAL);
    periodic.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = periodic.tick() => {}
            changed = changes.changed() => {
                if changed.is_err() { return; }
            }
        }
        if let Err(error) = run_once(
            &store,
            &runtime,
            &autonomy,
            &profile,
            &codex,
            &compute,
            &signals,
            &usage,
            &continuity,
            &conversation,
        )
        .await
        {
            tracing::warn!(target: crate::runtime_log::TARGET, event = "attacker_review_failed", %error, "adversarial external-input review failed");
            let mut snapshot = runtime.write().await;
            snapshot.phase = "error";
            snapshot.current_batch_size = 0;
            snapshot.last_error = Some(error.to_string());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    store: &AttackerStore,
    runtime: &Arc<RwLock<AttackerSnapshot>>,
    autonomy: &AutonomyStore,
    profile: &ProfileStore,
    codex: &Mutex<CodexClient>,
    compute: &ComputeStore,
    signals: &SignalStore,
    usage: &UsageStore,
    continuity: &ContinuityHost,
    conversation: &ConversationCoordinator,
) -> Result<()> {
    let config = autonomy.snapshot().await;
    let profile_snapshot = profile.snapshot().await;
    if !config.enabled || !config.attacker_enabled || profile_snapshot.status != SetupStatus::Ready
    {
        runtime.write().await.phase = "disabled";
        return Ok(());
    }
    if crate::exploration::quiet_end(Utc::now(), &config.quiet_hours).is_some() {
        runtime.write().await.phase = "quiet_hours";
        return Ok(());
    }
    if config.daily_token_limit > 0
        && usage
            .headline(&crate::exploration::today_started_at())
            .await?
            .autonomous_tokens_today
            >= config.daily_token_limit
    {
        runtime.write().await.phase = "token_limit";
        return Ok(());
    }
    let inputs = signals.attacker_inputs().await?;
    if store.initialize_existing(&inputs).await? {
        runtime.write().await.phase = "waiting";
        return Ok(());
    }
    let conversation_messages = continuity
        .recent_messages(MAX_INTERVENING_CONVERSATION_MESSAGES + 1)
        .await?;
    let pending = store.pending(&inputs, &conversation_messages).await;
    if !pending.skipped.is_empty() {
        // A source that has fallen behind the active dialogue must not remain
        // pending and surface after a later restart.
        store.complete(&pending.skipped, None).await?;
    }
    let batch = pending.review;
    let oldest_wait = batch
        .first()
        .and_then(|signal| DateTime::parse_from_rfc3339(&signal.observed_at).ok())
        .map(|at| {
            Utc::now()
                .signed_duration_since(at.with_timezone(&Utc))
                .num_minutes()
        })
        .unwrap_or_default();
    {
        let mut snapshot = runtime.write().await;
        snapshot.pending_signals = batch.len();
        snapshot.phase = "waiting";
    }
    if batch.len() < MIN_BATCH_SIZE && oldest_wait < MAX_WAIT_MINUTES {
        return Ok(());
    }
    let Some(input_events) = conversation.subscribe_background_input().await else {
        return Ok(());
    };
    let Ok(mut client) = codex.try_lock() else {
        return Ok(());
    };
    {
        let mut snapshot = runtime.write().await;
        snapshot.phase = "reviewing";
        snapshot.current_batch_size = batch.len();
        snapshot.last_error = None;
    }
    let packet = attacker_packet(&batch)?;
    let (events, mut event_rx) = mpsc::channel(16);
    let relay_runtime = Arc::clone(runtime);
    let relay = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            if let RuntimeEvent::Activity { .. } = event {
                relay_runtime.write().await.phase = "reviewing";
            }
        }
    });
    let outcome = client
        .review_attacker_signals(
            &packet,
            &compute.snapshot().await,
            &profile_snapshot,
            input_events,
            events,
        )
        .await?;
    relay.await.ok();
    if outcome.interrupted {
        runtime.write().await.phase = "waiting";
        return Ok(());
    }
    let mut published_issue = None;
    let batch_ids = batch
        .iter()
        .map(|signal| signal.id.as_str())
        .collect::<HashSet<_>>();
    if let Some(assessment) = outcome.assessment
        && assessment.disposition == AttackerDisposition::Challenge
        && !store.issue_was_published(&assessment.issue_key).await
        && assessment.message.trim().chars().count() >= 40
        && !assessment.sources.is_empty()
    {
        let related = assessment
            .related_signal_ids
            .into_iter()
            .filter(|id| batch_ids.contains(id.as_str()))
            .collect::<Vec<_>>();
        if !related.is_empty()
            && matches!(
                signals
                    .publish_attacker_challenge(
                        attacker_actor(&outcome.invocations),
                        &assessment.issue_key,
                        assessment.message,
                        assessment.reason,
                        related,
                        assessment.sources,
                    )
                    .await?,
                SignalPublishOutcome::Published
            )
        {
            published_issue = Some(assessment.issue_key);
        }
    }
    // Challenges remain transient input-role events rather than assistant
    // outreach. Account for their reasoning cost without consuming the normal
    // symbiont intervention/note quota.
    usage.record_all(&outcome.invocations).await?;
    store.complete(&batch, published_issue.as_deref()).await?;
    let mut snapshot = runtime.write().await;
    snapshot.phase = "waiting";
    snapshot.pending_signals = 0;
    snapshot.current_batch_size = 0;
    snapshot.last_reviewed_at = Some(timestamp(Utc::now()));
    if published_issue.is_some() {
        snapshot.last_published_at = snapshot.last_reviewed_at.clone();
    }
    Ok(())
}

fn attacker_actor(invocations: &[InvocationRecord]) -> InputRoleSnapshot {
    let run = invocations.last();
    InputRoleSnapshot {
        id: "symbiont_attacker".to_owned(),
        name: "symbiont-d · 异议".to_owned(),
        model: run
            .map(|run| run.model_display_name.clone())
            .unwrap_or_else(|| "Codex".to_owned()),
        effort: "adversarial".to_owned(),
        avatar_seed: "symbiont-dissent".to_owned(),
        provider_id: Some("codex".to_owned()),
        channel_id: Some("attacker".to_owned()),
    }
}

fn attacker_packet(batch: &[SignalEvent]) -> Result<String> {
    serde_json::to_string_pretty(
        &batch
            .iter()
            .map(|signal| {
                serde_json::json!({
                    "signal_id": signal.id,
                    "actor": signal.actor.name,
                    "title": signal.title,
                    "content": signal.content,
                    "received_text": signal.received_text,
                    "sources": signal.sources,
                    "event_at": signal.event_at,
                    "observed_at": signal.observed_at,
                })
            })
            .collect::<Vec<_>>(),
    )
    .context("encode attacker input packet")
}

pub fn attacker_prompt(packet: &str, completion_marker: &str) -> String {
    format!(
        r#"Privately examine a small batch of already-visible external inputs from an adversarial stance. These cards have already passed their own input gate. Never suppress, rank, rewrite, or invalidate them merely because they are unrelated to the user's projects. Your only task is to find a concrete claim, inference, causal story, benchmark framing, source conflict, or missing counterexample that deserves an evidence-backed challenge.

Use live web search freely when verification would help. Search primary or authoritative sources where possible. Do not publish generic skepticism, style criticism, a summary, a second feed item, or an objection based only on taste. A challenge must state what claim is too strong or misleading, what contrary evidence or boundary matters, and why that changes how the input should be read. The input can be interesting and still deserve correction.

Analyze often but speak sparingly. If no crisp, consequential and defensible rebuttal exists, submit `silent`. If one exists, submit exactly one `challenge` in concise natural Simplified Chinese. Include only signal IDs from the packet and concrete source URLs used for the challenge. Use a stable semantic `issue_key` so the Host can suppress repetition of the same dispute. Call `symbiont.submit_attacker_assessment` exactly once. This is a transient chat intervention: do not access or write PCP, infer user preferences, or pretend to be symbiont-d's ordinary conversational voice. After the tool call return exactly `{completion_marker}`.

<external-input-packet>
{packet}
</external-input-packet>"#
    )
}

pub fn attacker_assessment_from_invocations(
    invocations: &[InvocationRecord],
) -> Result<Option<AttackerAssessment>> {
    invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .rev()
        .find(|step| {
            step.succeeded
                && step.namespace == "symbiont"
                && step.tool == SUBMIT_ATTACKER_ASSESSMENT_TOOL
        })
        .map(|step| {
            serde_json::from_value(step.arguments.clone())
                .context("parse attacker assessment handoff")
        })
        .transpose()
}

fn trim_front<T>(values: &mut Vec<T>, limit: usize) {
    if values.len() > limit {
        values.drain(0..values.len() - limit);
    }
}

fn issue_is_in_cooldown(issue: &PublishedIssue, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&issue.published_at)
        .map(|published_at| {
            now.signed_duration_since(published_at.with_timezone(&Utc))
                .num_days()
                < ISSUE_COOLDOWN_DAYS
        })
        .unwrap_or(false)
}

fn is_near_current_conversation(signal: &SignalEvent, conversation: &[MemoryEntry]) -> bool {
    let Ok(signal_at) = DateTime::parse_from_rfc3339(&signal.observed_at) else {
        return false;
    };
    conversation
        .iter()
        .filter(|entry| matches!(entry.role, MemoryRole::User | MemoryRole::Assistant))
        .filter_map(|entry| DateTime::parse_from_rfc3339(&entry.at).ok())
        .filter(|entry_at| *entry_at > signal_at)
        .take(MAX_INTERVENING_CONVERSATION_MESSAGES + 1)
        .count()
        <= MAX_INTERVENING_CONVERSATION_MESSAGES
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn prompt_keeps_analysis_frequent_and_publication_restrained() {
        let prompt = attacker_prompt("[]", "<done/>");
        assert!(prompt.contains("Analyze often but speak sparingly"));
        assert!(prompt.contains("Never suppress"));
        assert!(prompt.contains("live web search"));
        assert!(prompt.contains("do not access or write PCP"));
    }

    #[test]
    fn issue_keys_expire_instead_of_suppressing_future_evidence_forever() {
        let recent = PublishedIssue {
            key: "benchmark-leakage".to_owned(),
            published_at: timestamp(Utc::now() - chrono::Duration::days(2)),
        };
        let old = PublishedIssue {
            key: "benchmark-leakage".to_owned(),
            published_at: timestamp(Utc::now() - chrono::Duration::days(8)),
        };
        assert!(issue_is_in_cooldown(&recent, Utc::now()));
        assert!(!issue_is_in_cooldown(&old, Utc::now()));
    }

    #[test]
    fn attacker_requires_the_source_to_remain_near_the_current_conversation() {
        let now = Utc::now();
        let signal = SignalEvent {
            id: "signal_conversation_distance".to_owned(),
            kind: crate::signals::SignalKind::ExternalInput,
            candidate_id: "candidate_conversation_distance".to_owned(),
            fingerprint: "fingerprint-conversation-distance".to_owned(),
            actor: InputRoleSnapshot::ambient("luna", "Luna", "gpt-test", "codex"),
            content: "A source input".to_owned(),
            received_text: "A source input".to_owned(),
            presentation: crate::sensing::SensingPresentation::Original,
            qualification_note: None,
            title: "An external source".to_owned(),
            summary: "An external source".to_owned(),
            sources: vec![],
            source_class: crate::sensing::SensingSourceClass::OpenDiscovery,
            event_at: Some(timestamp(now - chrono::Duration::days(12))),
            observed_at: timestamp(now - chrono::Duration::minutes(10)),
            review_reason: "visible in the chat".to_owned(),
            related_signal_ids: vec![],
            promoted_revision_id: None,
            briefing_topic: None,
            briefing_topic_status: crate::signals::BriefingTopicStatus::Unclassified,
            briefing_topic_reviewed: false,
            hidden: false,
            dismissed: false,
            duplicate_of_signal_id: None,
        };

        let message = |offset: i64| MemoryEntry {
            role: MemoryRole::User,
            at: timestamp(now - chrono::Duration::minutes(offset)),
            content: "later chat".to_owned(),
            revision_id: None,
            parts: Vec::new(),
            metadata: None,
            delivery_state: None,
        };
        assert!(is_near_current_conversation(
            &signal,
            &[message(1), message(2)]
        ));
        assert!(!is_near_current_conversation(
            &signal,
            &[message(1), message(2), message(3), message(4), message(5)],
        ));
    }

    #[tokio::test]
    async fn first_start_marks_existing_inputs_reviewed_without_replaying_history() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-attacker-{nonce}.json"));
        let store = AttackerStore::open(path.clone()).await.unwrap();
        let inputs = vec![SignalEvent {
            id: "signal_existing".to_owned(),
            kind: crate::signals::SignalKind::ExternalInput,
            candidate_id: "candidate_existing".to_owned(),
            fingerprint: "fingerprint-existing".to_owned(),
            actor: InputRoleSnapshot::ambient("luna", "Luna", "gpt-test", "codex"),
            content: "Existing input".to_owned(),
            received_text: "Existing input".to_owned(),
            presentation: crate::sensing::SensingPresentation::Original,
            qualification_note: None,
            title: "Existing input".to_owned(),
            summary: "Existing input".to_owned(),
            sources: vec![],
            source_class: crate::sensing::SensingSourceClass::OpenDiscovery,
            event_at: None,
            observed_at: timestamp(Utc::now()),
            review_reason: "already visible".to_owned(),
            related_signal_ids: vec![],
            promoted_revision_id: None,
            briefing_topic: None,
            briefing_topic_status: crate::signals::BriefingTopicStatus::Unclassified,
            briefing_topic_reviewed: false,
            hidden: false,
            dismissed: false,
            duplicate_of_signal_id: None,
        }];

        assert!(store.initialize_existing(&inputs).await.unwrap());
        assert!(store.pending(&inputs, &[]).await.review.is_empty());
        assert!(!store.initialize_existing(&inputs).await.unwrap());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn repeated_source_is_skipped_after_its_original_was_reviewed() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-attacker-source-{nonce}.json"));
        let store = AttackerStore::open(path.clone()).await.unwrap();
        let signal = |id: &str, title: &str, url: &str| SignalEvent {
            id: id.to_owned(),
            kind: crate::signals::SignalKind::ExternalInput,
            candidate_id: format!("candidate_{id}"),
            fingerprint: format!("v2|{id}"),
            actor: InputRoleSnapshot::ambient("luna", "Luna", "gpt-test", "codex"),
            content: title.to_owned(),
            received_text: title.to_owned(),
            presentation: crate::sensing::SensingPresentation::Original,
            qualification_note: None,
            title: title.to_owned(),
            summary: title.to_owned(),
            sources: vec![SensingSource {
                url: url.to_owned(),
                detail: "paper".to_owned(),
            }],
            source_class: crate::sensing::SensingSourceClass::Research,
            event_at: None,
            observed_at: timestamp(Utc::now()),
            review_reason: "credible".to_owned(),
            related_signal_ids: vec![],
            promoted_revision_id: None,
            briefing_topic: None,
            briefing_topic_status: crate::signals::BriefingTopicStatus::Unclassified,
            briefing_topic_reviewed: false,
            hidden: false,
            dismissed: false,
            duplicate_of_signal_id: None,
        };
        let original = signal(
            "signal_original",
            "The Collaboration Tax",
            "https://arxiv.org/abs/2608.22152",
        );
        let repeated = signal(
            "signal_repeated",
            "Coordination overhead revisited",
            "https://arxiv.org/pdf/2608.22152.pdf",
        );

        assert!(
            store
                .initialize_existing(std::slice::from_ref(&original))
                .await
                .unwrap()
        );
        let pending = store.pending(&[original, repeated.clone()], &[]).await;
        assert!(pending.review.is_empty());
        assert_eq!(pending.skipped.len(), 1);
        assert_eq!(pending.skipped[0].id, repeated.id);
        let _ = tokio::fs::remove_file(path).await;
    }
}
