use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::{
    sync::{Mutex, mpsc},
    time::sleep,
};
use tracing::{debug, warn};

use crate::{
    autonomy::AutonomyStore,
    codex::CodexClient,
    compute::ComputeStore,
    continuity::{ContinuityHost, MessageLinks},
    exploration::today_started_at,
    memory::{MemoryEntry, MemoryRole},
    profile::{ProfileStore, SetupStatus},
    reflection::ReflectionStore,
    symbiont_context::{ContextDocument, ContextDocumentKind, SymbiontContextStore},
    usage::UsageStore,
};

const INITIAL_DELAY: Duration = Duration::from_secs(45);
const RETRY_DELAY: Duration = Duration::from_secs(60);
const IDLE_DELAY: Duration = Duration::from_secs(300);
const PROFILE_REVIEW_INTERVAL_HOURS: i64 = 24;
const MAX_SOURCE_EVENTS: usize = 30;
const MAX_EVENT_CHARS: usize = 1_200;
const MAX_SOURCE_BUNDLE_CHARS: usize = 18_000;

pub fn start(
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
) {
    tokio::spawn(run(
        autonomy, profile, codex, compute, continuity, context, reflection, usage,
    ));
}

async fn run(
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    context: Arc<SymbiontContextStore>,
    reflection: Arc<ReflectionStore>,
    usage: Arc<UsageStore>,
) {
    sleep(INITIAL_DELAY).await;
    loop {
        let delay = match maintain_one(
            &autonomy,
            &profile,
            &codex,
            &compute,
            &continuity,
            &context,
            &reflection,
            &usage,
        )
        .await
        {
            Ok(MaintenanceState::Maintained | MaintenanceState::Busy) => RETRY_DELAY,
            Ok(MaintenanceState::Idle) => IDLE_DELAY,
            Err(error) => {
                warn!(%error, "Symbiont Context maintenance failed");
                RETRY_DELAY
            }
        };
        sleep(delay).await;
    }
}

enum MaintenanceState {
    Maintained,
    Busy,
    Idle,
}

async fn maintain_one(
    autonomy: &AutonomyStore,
    profile: &ProfileStore,
    codex: &Mutex<CodexClient>,
    compute: &ComputeStore,
    continuity: &ContinuityHost,
    context: &SymbiontContextStore,
    reflection: &ReflectionStore,
    usage: &UsageStore,
) -> Result<MaintenanceState> {
    let autonomy = autonomy.snapshot().await;
    let profile = profile.snapshot().await;
    if !autonomy.enabled || profile.status != SetupStatus::Ready {
        return Ok(MaintenanceState::Idle);
    }
    let headline = usage.headline(&today_started_at()).await?;
    if autonomy.daily_token_limit > 0
        && headline.autonomous_tokens_today >= autonomy.daily_token_limit
    {
        return Ok(MaintenanceState::Idle);
    }

    let recent = continuity.recent_messages(MAX_SOURCE_EVENTS).await?;
    let Some(latest_revision) = recent
        .iter()
        .rev()
        .find_map(|entry| entry.revision_id.as_deref())
    else {
        return Ok(MaintenanceState::Idle);
    };
    let snapshot = context.snapshot().await?;
    let map_is_current = snapshot
        .current_map
        .as_ref()
        .is_some_and(|document| document.has_source(latest_revision));
    let loops_are_current = snapshot
        .open_loops
        .as_ref()
        .is_some_and(|document| document.has_source(latest_revision));

    if !map_is_current || !loops_are_current {
        let source_bundle = conversation_source_bundle(&recent, &snapshot);
        return maintain_operational_context(
            codex,
            compute,
            continuity,
            context,
            reflection,
            usage,
            &profile,
            &source_bundle,
        )
        .await;
    }

    let Some(current_map) = snapshot.current_map.as_ref() else {
        return Ok(MaintenanceState::Idle);
    };
    let review_is_current = snapshot.profile_review.as_ref().is_some_and(|review| {
        review.has_source(&current_map.revision_id)
            || document_at_least_as_new_as(review, current_map)
    });
    if review_is_current
        || !profile_review_due(snapshot.profile_review.as_ref())
        || headline.autonomous_messages_today >= autonomy.daily_interrupt_limit as u64
    {
        return Ok(MaintenanceState::Idle);
    }

    let source_bundle = profile_source_bundle(&profile.orientation, &recent, &snapshot);
    review_profile(
        codex,
        compute,
        continuity,
        context,
        reflection,
        usage,
        &profile,
        &source_bundle,
    )
    .await
}

async fn maintain_operational_context(
    codex: &Mutex<CodexClient>,
    compute: &ComputeStore,
    continuity: &ContinuityHost,
    context: &SymbiontContextStore,
    reflection: &ReflectionStore,
    usage: &UsageStore,
    profile: &crate::profile::ProfileSnapshot,
    source_bundle: &str,
) -> Result<MaintenanceState> {
    let Ok(mut client) = codex.try_lock() else {
        return Ok(MaintenanceState::Busy);
    };
    let compute = compute.snapshot().await;
    let continuity_context = combined_context(continuity, context, reflection).await?;
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let event_drain = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });
    let outcome = client
        .maintain_symbiont_context(
            source_bundle,
            &compute,
            profile,
            &continuity_context,
            events_tx,
        )
        .await;
    drop(client);
    event_drain
        .await
        .context("join Symbiont Context maintenance event drain")?;
    let outcome = outcome?;
    usage.record_all(&outcome.invocations).await?;
    debug!(
        current_map_updated = outcome.current_map_updated,
        open_loops_updated = outcome.open_loops_updated,
        "Symbiont Context maintenance completed"
    );
    Ok(MaintenanceState::Maintained)
}

async fn review_profile(
    codex: &Mutex<CodexClient>,
    compute: &ComputeStore,
    continuity: &ContinuityHost,
    context: &SymbiontContextStore,
    reflection: &ReflectionStore,
    usage: &UsageStore,
    profile: &crate::profile::ProfileSnapshot,
    source_bundle: &str,
) -> Result<MaintenanceState> {
    let Ok(mut client) = codex.try_lock() else {
        return Ok(MaintenanceState::Busy);
    };
    let compute = compute.snapshot().await;
    let continuity_context = combined_context(continuity, context, reflection).await?;
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let event_drain = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });
    let outcome = client
        .review_profile(
            source_bundle,
            &compute,
            profile,
            &continuity_context,
            events_tx,
        )
        .await;
    drop(client);
    event_drain
        .await
        .context("join profile review event drain")?;
    let outcome = outcome?;
    usage.record_all(&outcome.invocations).await?;

    if let Some(question) = outcome.clarification_question.as_deref() {
        let mut input_revision_ids = outcome.context_revision_ids;
        if let Some(review) = context.read(ContextDocumentKind::ProfileReview).await? {
            input_revision_ids.push(review.revision_id);
        }
        input_revision_ids.sort();
        input_revision_ids.dedup();
        continuity
            .ingest_message(
                MemoryRole::Assistant,
                question,
                Vec::new(),
                Some(outcome.metadata),
                MessageLinks {
                    responds_to: None,
                    input_revision_ids,
                },
            )
            .await?;
    }
    debug!(
        status = outcome.status.as_deref().unwrap_or("missing"),
        asked = outcome.clarification_question.is_some(),
        "profile review completed"
    );
    Ok(MaintenanceState::Maintained)
}

async fn combined_context(
    continuity: &ContinuityHost,
    context: &SymbiontContextStore,
    reflection: &ReflectionStore,
) -> Result<String> {
    Ok(format!(
        "{}\n\n{}\n\n{}",
        continuity.context_seed(None).await,
        context.prompt().await?,
        reflection.prompt().await?
    ))
}

fn conversation_source_bundle(
    recent: &[MemoryEntry],
    snapshot: &crate::symbiont_context::SymbiontContextSnapshot,
) -> String {
    let mut sections = vec![format_recent_events(recent)];
    if let Some(document) = snapshot.current_map.as_ref() {
        sections.push(format_document("previous-current-map", document));
    }
    if let Some(document) = snapshot.open_loops.as_ref() {
        sections.push(format_document("previous-open-loops", document));
    }
    truncate_bundle(sections.join("\n\n"))
}

fn profile_source_bundle(
    orientation: &str,
    recent: &[MemoryEntry],
    snapshot: &crate::symbiont_context::SymbiontContextSnapshot,
) -> String {
    let mut sections = vec![format!(
        "<orientation>\n{}\n</orientation>",
        orientation.trim()
    )];
    if let Some(document) = snapshot.current_map.as_ref() {
        sections.push(format_document("current-map", document));
    }
    if let Some(document) = snapshot.open_loops.as_ref() {
        sections.push(format_document("open-loops", document));
    }
    sections.push(format_recent_events(recent));
    truncate_bundle(sections.join("\n\n"))
}

fn format_recent_events(entries: &[MemoryEntry]) -> String {
    let mut lines = vec!["<recent-events>".to_owned()];
    for entry in entries {
        let Some(revision_id) = entry.revision_id.as_deref() else {
            continue;
        };
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
        let mut content = entry
            .content
            .chars()
            .take(MAX_EVENT_CHARS)
            .collect::<String>();
        if entry.content.chars().count() > MAX_EVENT_CHARS {
            content.push_str(" [truncated]");
        }
        lines.push(format!(
            "<event revision=\"{revision_id}\" role=\"{role}\" origin=\"{origin}\">\n{content}\n</event>"
        ));
    }
    lines.push("</recent-events>".to_owned());
    lines.join("\n")
}

fn format_document(label: &str, document: &ContextDocument) -> String {
    format!(
        "<{label} revision=\"{}\">\n{}\n</{label}>",
        document.revision_id, document.content
    )
}

fn truncate_bundle(bundle: String) -> String {
    if bundle.chars().count() <= MAX_SOURCE_BUNDLE_CHARS {
        return bundle;
    }
    let mut truncated = bundle
        .chars()
        .take(MAX_SOURCE_BUNDLE_CHARS)
        .collect::<String>();
    truncated.push_str("\n[older source material truncated; use PCP for Detail]");
    truncated
}

fn profile_review_due(review: Option<&ContextDocument>) -> bool {
    let Some(review) = review else {
        return true;
    };
    let Ok(updated_at) = DateTime::parse_from_rfc3339(&review.updated_at) else {
        return true;
    };
    Utc::now() - updated_at.with_timezone(&Utc)
        >= chrono::Duration::hours(PROFILE_REVIEW_INTERVAL_HOURS)
}

fn document_at_least_as_new_as(left: &ContextDocument, right: &ContextDocument) -> bool {
    let Ok(left_at) = DateTime::parse_from_rfc3339(&left.updated_at) else {
        return false;
    };
    let Ok(right_at) = DateTime::parse_from_rfc3339(&right.updated_at) else {
        return false;
    };
    left_at >= right_at
}
