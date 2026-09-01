//! Explicit, resumable repair of transcript-backed PCP Pages.
//!
//! Preview uses the ordinary read-only tenant surface and stores a bounded,
//! model-reviewed ledger. Apply opens the separate repair enrollment only for
//! the duration of this workflow, rechecks the optimistic-lock revision, and
//! records a new immutable Revision through PCP's repair API.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{
    BrowseIndexOrder, PagePayload, Projection, ReadPagesRequest, RepairPageRequest, SourceRef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    sync::{mpsc, watch},
};
use tracing::{info, warn};

use crate::{
    codex::{CodexClient, PcpHistoryRepairProposal, PcpHistoryRepairRequest},
    compute::{ComputeLane, ComputeStore},
    continuity::ContinuityHost,
    pcp_connection,
    profile::ProfileStore,
    transcript::{
        TranscriptRecall, TranscriptSearchMessage, TranscriptSourceOptions, TranscriptSourceStatus,
        TranscriptStore,
    },
};

const LEDGER_VERSION: u32 = 1;
const INVENTORY_PAGE_SIZE: u32 = 50;
const DEFAULT_MAX_PAGES_PER_RUN: usize = 32;
const MAX_PAGES_PER_RUN: usize = 256;
const REVIEW_BATCH_SIZE: usize = 4;
const MAX_PAGE_CONTENT_CHARS: usize = 64_000;
const MAX_REASON_CHARS: usize = 2_000;
const TRANSCRIPT_PROVIDER_ID: &str = "symbiont:transcript";
const TRANSCRIPT_LOCATOR_PREFIX: &str = "message/";
const HISTORY_REPAIR_TOOL_ID: &str = "symbiont-d:pcp-history-repair";
const LANGUAGE_REPAIR_TOOL_ID: &str = "symbiont-d:pcp-language-repair";
const HISTORY_REPAIR_KIND: &str = "content-fidelity";
const LANGUAGE_REPAIR_KIND: &str = "language-fidelity-zh";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunMode {
    Preview,
    Apply,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairTask {
    ContentFidelity,
    ChineseLanguageFidelity,
}

impl RepairTask {
    fn as_str(self) -> &'static str {
        match self {
            Self::ContentFidelity => HISTORY_REPAIR_KIND,
            Self::ChineseLanguageFidelity => LANGUAGE_REPAIR_KIND,
        }
    }

    fn tool_id(self) -> &'static str {
        match self {
            Self::ContentFidelity => HISTORY_REPAIR_TOOL_ID,
            Self::ChineseLanguageFidelity => LANGUAGE_REPAIR_TOOL_ID,
        }
    }

    fn ledger_file(self) -> &'static str {
        match self {
            Self::ContentFidelity => "pcp-history-repair.json",
            Self::ChineseLanguageFidelity => "pcp-language-repair.json",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepairRun {
    pub mode: RunMode,
    pub task: RepairTask,
}

impl RepairRun {
    pub fn ledger_file(self) -> &'static str {
        self.task.ledger_file()
    }
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RepairReport {
    pub pcp_identity_id: String,
    pub candidates_found: usize,
    pub reviewed: usize,
    pub proposed_revisions: usize,
    pub kept: usize,
    pub escalated: usize,
    pub applied: usize,
    pub stale: usize,
    pub unresolved_sources: usize,
    pub remaining: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepairLedger {
    version: u32,
    pcp_identity_id: String,
    #[serde(default = "default_history_repair_kind")]
    repair_kind: String,
    updated_at: String,
    #[serde(default)]
    entries: Vec<RepairLedgerEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepairLedgerEntry {
    page_id: String,
    expected_revision_id: String,
    kind: String,
    media_type: String,
    action: String,
    reason: String,
    content: String,
    source_message_ids: Vec<String>,
    #[serde(default = "default_review_lane")]
    review_lane: String,
    #[serde(default)]
    preserved_source_refs: Vec<SourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    facets: Option<Value>,
    reviewed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applied_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apply_note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewCandidate {
    page_id: String,
    expected_revision_id: String,
    kind: String,
    media_type: String,
    current_content: String,
    original_source_message_ids: Vec<String>,
    unavailable_source_message_ids: Vec<String>,
    source_messages: Vec<TranscriptSearchMessage>,
}

#[derive(Clone)]
struct CandidateState {
    review: ReviewCandidate,
    preserved_source_refs: Vec<SourceRef>,
    facets: Option<Value>,
}

pub fn requested_run() -> Result<Option<RepairRun>> {
    let history = env::var_os("SYMBIONT_RUN_PCP_HISTORY_REPAIR");
    let language = env::var_os("SYMBIONT_RUN_PCP_LANGUAGE_REPAIR");
    anyhow::ensure!(
        history.is_none() || language.is_none(),
        "request only one PCP repair task at a time"
    );
    let Some((value, task, variable)) = history
        .map(|value| {
            (
                value,
                RepairTask::ContentFidelity,
                "SYMBIONT_RUN_PCP_HISTORY_REPAIR",
            )
        })
        .or_else(|| {
            language.map(|value| {
                (
                    value,
                    RepairTask::ChineseLanguageFidelity,
                    "SYMBIONT_RUN_PCP_LANGUAGE_REPAIR",
                )
            })
        })
    else {
        return Ok(None);
    };
    let mode = match value.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "preview" => RunMode::Preview,
        "apply" => RunMode::Apply,
        _ => anyhow::bail!("{variable} must be exactly `preview` or `apply`"),
    };
    Ok(Some(RepairRun { mode, task }))
}

pub async fn run_preview(
    task: RepairTask,
    ledger_path: PathBuf,
    codex: &mut CodexClient,
    transcript: Arc<TranscriptStore>,
    continuity: Arc<ContinuityHost>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
) -> Result<RepairReport> {
    let identity_id = continuity.pcp_identity_id().to_owned();
    let mut ledger = load_ledger(&ledger_path, &identity_id, task).await?;
    let max_pages = max_pages_per_run()?;
    let mut report = RepairReport {
        pcp_identity_id: identity_id,
        ..RepairReport::default()
    };
    preview(
        &ledger_path,
        &mut ledger,
        task,
        codex,
        transcript,
        continuity,
        compute,
        profile,
        max_pages,
        &mut report,
    )
    .await?;
    Ok(report)
}

pub async fn run_apply(
    task: RepairTask,
    workspace: &Path,
    ledger_path: PathBuf,
    transcript: Arc<TranscriptStore>,
    continuity: Arc<ContinuityHost>,
) -> Result<RepairReport> {
    let identity_id = continuity.pcp_identity_id().to_owned();
    let mut ledger = load_ledger(&ledger_path, &identity_id, task).await?;
    let max_pages = max_pages_per_run()?;
    let mut report = RepairReport {
        pcp_identity_id: identity_id,
        ..RepairReport::default()
    };
    apply(
        workspace,
        &ledger_path,
        &mut ledger,
        task,
        transcript,
        continuity,
        max_pages,
        &mut report,
    )
    .await?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn preview(
    ledger_path: &Path,
    ledger: &mut RepairLedger,
    task: RepairTask,
    codex: &mut CodexClient,
    transcript: Arc<TranscriptStore>,
    continuity: Arc<ContinuityHost>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    max_pages: usize,
    report: &mut RepairReport,
) -> Result<()> {
    let current_keys = ledger
        .entries
        .iter()
        .flat_map(|entry| {
            let mut keys = vec![entry_key(&entry.page_id, &entry.expected_revision_id)];
            if let Some(revision_id) = &entry.applied_revision_id {
                keys.push(entry_key(&entry.page_id, revision_id));
            }
            keys
        })
        .collect::<HashSet<_>>();
    let recall = TranscriptRecall::new(transcript);
    let mut candidates = inventory_candidates(&continuity, &recall, report).await?;
    if task == RepairTask::ChineseLanguageFidelity {
        candidates.retain(is_chinese_language_repair_candidate);
    }
    report.candidates_found = candidates.len();
    candidates.retain(|candidate| {
        !current_keys.contains(&entry_key(
            &candidate.review.page_id,
            &candidate.review.expected_revision_id,
        ))
    });
    report.remaining = candidates.len().saturating_sub(max_pages);
    candidates.truncate(max_pages);

    for batch in candidates.chunks(REVIEW_BATCH_SIZE) {
        let reviewed = review_with_escalation(
            codex,
            batch,
            task,
            Arc::clone(&compute),
            Arc::clone(&profile),
        )
        .await?;
        report.escalated += reviewed.critical_page_ids.len();
        for (candidate, proposal) in batch.iter().zip(reviewed.proposals) {
            let is_revision = proposal.action == "revise";
            let review_lane = if reviewed.critical_page_ids.contains(&proposal.page_id) {
                ComputeLane::Critical.as_str()
            } else {
                ComputeLane::Conversation.as_str()
            };
            ledger.entries.push(RepairLedgerEntry {
                page_id: proposal.page_id,
                expected_revision_id: proposal.expected_revision_id,
                kind: candidate.review.kind.clone(),
                media_type: candidate.review.media_type.clone(),
                action: proposal.action,
                reason: proposal.reason,
                content: proposal.content,
                source_message_ids: proposal.source_message_ids,
                review_lane: review_lane.to_owned(),
                preserved_source_refs: candidate.preserved_source_refs.clone(),
                facets: candidate.facets.clone(),
                reviewed_at: now(),
                applied_revision_id: None,
                apply_note: None,
            });
            report.reviewed += 1;
            if is_revision {
                report.proposed_revisions += 1;
            } else {
                report.kept += 1;
            }
        }
        save_ledger(ledger_path, ledger).await?;
        info!(
            reviewed = report.reviewed,
            proposed_revisions = report.proposed_revisions,
            escalated = report.escalated,
            "persisted one PCP history repair preview batch"
        );
    }
    save_ledger(ledger_path, ledger).await?;
    Ok(())
}

struct ReviewedBatch {
    proposals: Vec<PcpHistoryRepairProposal>,
    critical_page_ids: HashSet<String>,
}

async fn review_with_escalation(
    codex: &mut CodexClient,
    batch: &[CandidateState],
    task: RepairTask,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
) -> Result<ReviewedBatch> {
    let primary = invoke_reviewer(
        codex,
        batch,
        task,
        ComputeLane::Conversation,
        true,
        Arc::clone(&compute),
        Arc::clone(&profile),
    )
    .await?;
    let critical_candidates = batch
        .iter()
        .zip(&primary)
        .filter(|(_, proposal)| proposal.action == "escalate")
        .map(|(candidate, _)| candidate.clone())
        .collect::<Vec<_>>();
    let critical_page_ids = critical_candidates
        .iter()
        .map(|candidate| candidate.review.page_id.clone())
        .collect::<HashSet<_>>();
    if critical_candidates.is_empty() {
        return Ok(ReviewedBatch {
            proposals: primary,
            critical_page_ids,
        });
    }

    let critical = invoke_reviewer(
        codex,
        &critical_candidates,
        task,
        ComputeLane::Critical,
        false,
        compute,
        profile,
    )
    .await?;
    let mut critical_by_page = critical
        .into_iter()
        .map(|proposal| (proposal.page_id.clone(), proposal))
        .collect::<BTreeMap<_, _>>();
    let proposals = primary
        .into_iter()
        .map(|proposal| {
            if proposal.action == "escalate" {
                critical_by_page.remove(&proposal.page_id).with_context(|| {
                    format!(
                        "critical PCP repair review omitted Page {}",
                        proposal.page_id
                    )
                })
            } else {
                Ok(proposal)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        critical_by_page.is_empty(),
        "critical PCP repair review added unknown Pages"
    );
    Ok(ReviewedBatch {
        proposals,
        critical_page_ids,
    })
}

async fn invoke_reviewer(
    codex: &mut CodexClient,
    batch: &[CandidateState],
    task: RepairTask,
    lane: ComputeLane,
    allow_escalation: bool,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
) -> Result<Vec<PcpHistoryRepairProposal>> {
    let bundle =
        serde_json::to_string_pretty(&batch.iter().map(|item| &item.review).collect::<Vec<_>>())?;
    let compute_snapshot = compute.snapshot().await;
    let profile_snapshot = profile.snapshot().await;
    let mut rejection_reason = None;
    for attempt in 0..2 {
        let (_input_tx, input_events) = watch::channel(0_u64);
        let (event_tx, mut event_rx) = mpsc::channel(256);
        let event_drain = tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
        let outcome = codex
            .review_pcp_history_repair_batch(PcpHistoryRepairRequest {
                batch_bundle: &bundle,
                language_fidelity: task == RepairTask::ChineseLanguageFidelity,
                lane,
                allow_escalation,
                rejection_reason: rejection_reason.as_deref(),
                compute: &compute_snapshot,
                profile: &profile_snapshot,
                input_events,
                events: event_tx,
            })
            .await;
        event_drain.await.context("join PCP repair event drain")?;
        let outcome = outcome.with_context(|| {
            format!(
                "review one PCP history repair batch at {} lane",
                lane.as_str()
            )
        })?;
        match validate_proposals(batch, outcome.proposals, task, allow_escalation) {
            Ok(proposals) => return Ok(proposals),
            Err(error) if attempt == 0 => rejection_reason = Some(error.to_string()),
            Err(error) => {
                return Err(error).context(
                    "validate PCP history repair review after one semantic corrective retry",
                );
            }
        }
    }
    unreachable!("PCP history repair semantic retry loop always returns")
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    workspace: &Path,
    ledger_path: &Path,
    ledger: &mut RepairLedger,
    task: RepairTask,
    transcript: Arc<TranscriptStore>,
    continuity: Arc<ContinuityHost>,
    max_pages: usize,
    report: &mut RepairReport,
) -> Result<()> {
    report.candidates_found = ledger.entries.len();
    report.proposed_revisions = ledger
        .entries
        .iter()
        .filter(|entry| entry.action == "revise")
        .count();
    report.kept = ledger
        .entries
        .iter()
        .filter(|entry| entry.action == "keep")
        .count();
    let pending = ledger
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.action == "revise"
                && entry.applied_revision_id.is_none()
                && entry.apply_note.is_none()
        })
        .map(|(index, _)| index)
        .take(max_pages)
        .collect::<Vec<_>>();
    report.remaining = ledger
        .entries
        .iter()
        .filter(|entry| {
            entry.action == "revise"
                && entry.applied_revision_id.is_none()
                && entry.apply_note.is_none()
        })
        .count()
        .saturating_sub(pending.len());
    if pending.is_empty() {
        return Ok(());
    }

    let repair_client = pcp_connection::open_repair(workspace).await?;
    anyhow::ensure!(
        repair_client.identity_id() == ledger.pcp_identity_id,
        "repair enrollment resolved a different PCP Store identity"
    );
    let recall = TranscriptRecall::new(transcript);
    for index in pending {
        let entry = ledger.entries[index].clone();
        validate_apply_entry(&entry, task)?;
        let current_revision = continuity.current_revision_id(&entry.page_id).await?;
        if current_revision != entry.expected_revision_id {
            ledger.entries[index].apply_note = Some(format!(
                "stale: expected {}, current {}",
                entry.expected_revision_id, current_revision
            ));
            report.stale += 1;
            save_ledger(ledger_path, ledger).await?;
            warn!(
                page_id = entry.page_id,
                expected_revision_id = entry.expected_revision_id,
                current_revision_id = current_revision,
                "skipped stale PCP history repair"
            );
            continue;
        }
        validate_active_sources(&recall, &entry.source_message_ids).await?;
        let mut source_refs = continuity
            .transcript_source_refs(&entry.source_message_ids)
            .await?;
        source_refs.extend(entry.preserved_source_refs.clone());
        let result = repair_client
            .repair_page(RepairPageRequest {
                page_id: entry.page_id.clone(),
                expected_revision_id: entry.expected_revision_id.clone(),
                reason: entry.reason.clone(),
                payload: Some(PagePayload {
                    media_type: entry.media_type.clone(),
                    content: entry.content.clone(),
                }),
                source_refs,
                facets: entry.facets.clone(),
                based_on_revision_ids: Vec::new(),
                tool_or_model: Some(task.tool_id().to_owned()),
                idempotency_key: Some(repair_idempotency_key(&entry, task)),
            })
            .await
            .with_context(|| format!("repair PCP Page {}", entry.page_id))?;
        ledger.entries[index].applied_revision_id = Some(result.revision_id.clone());
        ledger.entries[index].apply_note = Some("applied".to_owned());
        report.applied += 1;
        save_ledger(ledger_path, ledger).await?;
        info!(
            page_id = entry.page_id,
            revision_id = result.revision_id,
            "applied one audited PCP history repair"
        );
    }
    Ok(())
}

async fn inventory_candidates(
    continuity: &ContinuityHost,
    recall: &TranscriptRecall,
    report: &mut RepairReport,
) -> Result<Vec<CandidateState>> {
    let mut cursor = None;
    let mut candidates = Vec::new();
    loop {
        let page = continuity
            .store()
            .browse_content_pages(
                vec![continuity.pcp_scope().to_owned()],
                None,
                BrowseIndexOrder::Oldest,
                INVENTORY_PAGE_SIZE,
                cursor,
                32_000,
                pcp_client::ContentLibraryFilter::default(),
            )
            .await?;
        for hit in page.hits {
            let Some(read) = continuity
                .read(ReadPagesRequest {
                    page_ids: vec![hit.page_id],
                    revision_ids: Vec::new(),
                    projections: vec![
                        Projection::Manifest,
                        Projection::Payload,
                        Projection::Sources,
                        Projection::Facets,
                    ],
                    max_chars: MAX_PAGE_CONTENT_CHARS as u32,
                })
                .await?
                .into_iter()
                .next()
            else {
                continue;
            };
            let transcript_ids = transcript_message_ids(&read.revision.source_refs);
            if transcript_ids.is_empty() {
                continue;
            }
            let Some(payload) = read.revision.payload else {
                continue;
            };
            let mut messages = BTreeMap::new();
            let mut unavailable = Vec::new();
            for message_id in &transcript_ids {
                let resolution = recall
                    .resolve_source(
                        message_id,
                        TranscriptSourceOptions {
                            context_before: 1,
                            context_after: 1,
                            ..TranscriptSourceOptions::default()
                        },
                    )
                    .await?;
                if resolution.status != TranscriptSourceStatus::Active {
                    unavailable.push(message_id.clone());
                    report.unresolved_sources += 1;
                    continue;
                }
                for message in resolution.messages {
                    messages.entry(message.sequence).or_insert(message);
                }
            }
            if messages.is_empty() {
                continue;
            }
            let preserved_source_refs = read
                .revision
                .source_refs
                .iter()
                .filter(|source| source.provider_id != TRANSCRIPT_PROVIDER_ID)
                .cloned()
                .collect();
            candidates.push(CandidateState {
                review: ReviewCandidate {
                    page_id: read.page.page_id,
                    expected_revision_id: read.revision.revision_id,
                    kind: read.page.kind,
                    media_type: payload.media_type,
                    current_content: payload.content,
                    original_source_message_ids: transcript_ids,
                    unavailable_source_message_ids: unavailable,
                    source_messages: messages.into_values().collect(),
                },
                preserved_source_refs,
                facets: read.revision.facets,
            });
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(candidates)
}

fn is_chinese_language_repair_candidate(candidate: &CandidateState) -> bool {
    if !is_english_dominant(&candidate.review.current_content) {
        return false;
    }
    let direct_ids = candidate
        .review
        .original_source_message_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let direct_messages = candidate
        .review
        .source_messages
        .iter()
        .filter(|message| direct_ids.contains(message.message_id.as_str()))
        .collect::<Vec<_>>();
    let user_content = direct_messages
        .iter()
        .filter(|message| matches!(message.role, crate::memory::MemoryRole::User))
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    let source_content = if user_content.is_empty() {
        direct_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
    } else {
        user_content
    };
    chinese_character_count(&source_content.join("\n")) >= 10
}

fn is_english_dominant(content: &str) -> bool {
    let chinese_characters = chinese_character_count(content);
    let ascii_letters = content.chars().filter(char::is_ascii_alphabetic).count();
    ascii_letters > 80 && ascii_letters > chinese_characters.saturating_mul(4)
}

fn chinese_character_count(content: &str) -> usize {
    content
        .chars()
        .filter(|character| is_cjk(*character))
        .count()
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
    )
}

fn validate_proposals(
    batch: &[CandidateState],
    proposals: Vec<PcpHistoryRepairProposal>,
    task: RepairTask,
    allow_escalation: bool,
) -> Result<Vec<PcpHistoryRepairProposal>> {
    anyhow::ensure!(
        proposals.len() == batch.len(),
        "PCP repair reviewer must return exactly one proposal per candidate"
    );
    let mut by_page = proposals
        .into_iter()
        .map(|proposal| (proposal.page_id.clone(), proposal))
        .collect::<BTreeMap<_, _>>();
    anyhow::ensure!(
        by_page.len() == batch.len(),
        "PCP repair reviewer returned duplicate Page proposals"
    );
    let mut validated = Vec::with_capacity(batch.len());
    for candidate in batch {
        let mut proposal = by_page.remove(&candidate.review.page_id).with_context(|| {
            format!(
                "PCP repair reviewer omitted Page {}",
                candidate.review.page_id
            )
        })?;
        anyhow::ensure!(
            proposal.expected_revision_id == candidate.review.expected_revision_id,
            "PCP repair reviewer changed expected Revision for {}",
            candidate.review.page_id
        );
        let supported_action = match task {
            RepairTask::ContentFidelity => {
                matches!(proposal.action.as_str(), "keep" | "revise")
                    || (allow_escalation && proposal.action == "escalate")
            }
            RepairTask::ChineseLanguageFidelity => {
                proposal.action == "revise" || (allow_escalation && proposal.action == "escalate")
            }
        };
        anyhow::ensure!(
            supported_action,
            "PCP repair reviewer returned an unsupported action"
        );
        let reason_chars = proposal.reason.trim().chars().count();
        anyhow::ensure!(
            (1..=MAX_REASON_CHARS).contains(&reason_chars),
            "PCP repair reason is empty or too long"
        );
        if matches!(proposal.action.as_str(), "keep" | "escalate") {
            proposal
                .content
                .clone_from(&candidate.review.current_content);
        }
        let content_chars = proposal.content.chars().count();
        anyhow::ensure!(
            (1..=MAX_PAGE_CONTENT_CHARS).contains(&content_chars),
            "PCP repair content is empty or too long"
        );
        let allowed = candidate
            .review
            .source_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<HashSet<_>>();
        let selected = proposal
            .source_message_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            !selected.is_empty() && selected.iter().all(|id| allowed.contains(id)),
            "PCP repair proposal cited a missing or unreviewed transcript source"
        );
        if task == RepairTask::ChineseLanguageFidelity {
            let expected = candidate
                .review
                .original_source_message_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            anyhow::ensure!(
                proposal.source_message_ids.len() == expected.len() && selected == expected,
                "PCP language repair must preserve the exact direct transcript source set"
            );
        }
        if proposal.action == "revise" {
            anyhow::ensure!(
                proposal.content != candidate.review.current_content,
                "PCP repair reviewer proposed a no-op revision"
            );
            if task == RepairTask::ChineseLanguageFidelity {
                anyhow::ensure!(
                    chinese_character_count(&proposal.content) >= 10
                        && !is_english_dominant(&proposal.content),
                    "PCP language repair content is not meaningfully expressed in Chinese"
                );
            }
        }
        validated.push(proposal);
    }
    anyhow::ensure!(
        by_page.is_empty(),
        "PCP repair reviewer added unknown Pages"
    );
    Ok(validated)
}

async fn validate_active_sources(recall: &TranscriptRecall, message_ids: &[String]) -> Result<()> {
    for message_id in message_ids {
        let resolution = recall
            .resolve_source(
                message_id,
                TranscriptSourceOptions {
                    context_before: 0,
                    context_after: 0,
                    ..TranscriptSourceOptions::default()
                },
            )
            .await?;
        anyhow::ensure!(
            resolution.status == TranscriptSourceStatus::Active,
            "transcript source {message_id} is no longer active"
        );
    }
    Ok(())
}

fn validate_apply_entry(entry: &RepairLedgerEntry, task: RepairTask) -> Result<()> {
    anyhow::ensure!(
        entry.action == "revise",
        "only revise proposals can be applied"
    );
    anyhow::ensure!(
        !entry.page_id.trim().is_empty() && !entry.expected_revision_id.trim().is_empty(),
        "repair ledger Page and Revision identifiers are required"
    );
    let reason_chars = entry.reason.trim().chars().count();
    anyhow::ensure!(
        (1..=MAX_REASON_CHARS).contains(&reason_chars),
        "repair ledger reason is empty or too long"
    );
    let content_chars = entry.content.chars().count();
    anyhow::ensure!(
        (1..=MAX_PAGE_CONTENT_CHARS).contains(&content_chars),
        "repair ledger content is empty or too long"
    );
    anyhow::ensure!(
        !entry.media_type.trim().is_empty() && entry.media_type.chars().count() <= 128,
        "repair ledger media type is invalid"
    );
    if task == RepairTask::ChineseLanguageFidelity {
        anyhow::ensure!(
            chinese_character_count(&entry.content) >= 10 && !is_english_dominant(&entry.content),
            "language repair ledger content is not meaningfully expressed in Chinese"
        );
    }
    let unique_sources = entry
        .source_message_ids
        .iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        !unique_sources.is_empty()
            && unique_sources.len() == entry.source_message_ids.len()
            && unique_sources.len() <= 100,
        "repair ledger transcript sources are empty, duplicated, or too numerous"
    );
    Ok(())
}

fn transcript_message_ids(source_refs: &[SourceRef]) -> Vec<String> {
    let mut ids = source_refs
        .iter()
        .filter(|source| source.provider_id == TRANSCRIPT_PROVIDER_ID)
        .filter_map(|source| source.locator.strip_prefix(TRANSCRIPT_LOCATOR_PREFIX))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn max_pages_per_run() -> Result<usize> {
    let Some(value) = env::var_os("SYMBIONT_PCP_HISTORY_REPAIR_MAX_PAGES") else {
        return Ok(DEFAULT_MAX_PAGES_PER_RUN);
    };
    let parsed = value
        .to_string_lossy()
        .parse::<usize>()
        .context("parse SYMBIONT_PCP_HISTORY_REPAIR_MAX_PAGES")?;
    anyhow::ensure!(
        (1..=MAX_PAGES_PER_RUN).contains(&parsed),
        "SYMBIONT_PCP_HISTORY_REPAIR_MAX_PAGES must be between 1 and {MAX_PAGES_PER_RUN}"
    );
    Ok(parsed)
}

async fn load_ledger(path: &Path, identity_id: &str, task: RepairTask) -> Result<RepairLedger> {
    match fs::read(path).await {
        Ok(bytes) => {
            let ledger = serde_json::from_slice::<RepairLedger>(&bytes)
                .with_context(|| format!("decode PCP repair ledger {}", path.display()))?;
            anyhow::ensure!(
                ledger.version == LEDGER_VERSION,
                "unsupported PCP repair ledger"
            );
            anyhow::ensure!(
                ledger.pcp_identity_id == identity_id,
                "PCP repair ledger belongs to a different Store identity"
            );
            anyhow::ensure!(
                ledger.repair_kind == task.as_str(),
                "PCP repair ledger belongs to a different repair task"
            );
            Ok(ledger)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RepairLedger {
            version: LEDGER_VERSION,
            pcp_identity_id: identity_id.to_owned(),
            repair_kind: task.as_str().to_owned(),
            updated_at: now(),
            entries: Vec::new(),
        }),
        Err(error) => {
            Err(error).with_context(|| format!("read PCP repair ledger {}", path.display()))
        }
    }
}

async fn save_ledger(path: &Path, ledger: &mut RepairLedger) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create PCP repair ledger directory {}", parent.display()))?;
    }
    ledger.updated_at = now();
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(ledger)?)
        .await
        .with_context(|| format!("write PCP repair ledger {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("publish PCP repair ledger {}", path.display()))?;
    Ok(())
}

fn entry_key(page_id: &str, revision_id: &str) -> String {
    format!("{page_id}@{revision_id}")
}

fn repair_idempotency_key(entry: &RepairLedgerEntry, task: RepairTask) -> String {
    let mut digest = Sha256::new();
    digest.update(task.as_str().as_bytes());
    digest.update([0]);
    digest.update(entry.page_id.as_bytes());
    digest.update([0]);
    digest.update(entry.expected_revision_id.as_bytes());
    digest.update([0]);
    digest.update(entry.content.as_bytes());
    for source_id in &entry.source_message_ids {
        digest.update([0]);
        digest.update(source_id.as_bytes());
    }
    format!(
        "symbiont-pcp-repair:{}:{:x}",
        task.as_str(),
        digest.finalize()
    )
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn default_review_lane() -> String {
    ComputeLane::Critical.as_str().to_owned()
}

fn default_history_repair_kind() -> String {
    HISTORY_REPAIR_KIND.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_candidate(content: &str) -> CandidateState {
        CandidateState {
            review: ReviewCandidate {
                page_id: "page".to_owned(),
                expected_revision_id: "revision".to_owned(),
                kind: "note".to_owned(),
                media_type: "text/markdown".to_owned(),
                current_content: content.to_owned(),
                original_source_message_ids: vec!["message".to_owned()],
                unavailable_source_message_ids: Vec::new(),
                source_messages: vec![TranscriptSearchMessage {
                    message_id: "message".to_owned(),
                    sequence: 1,
                    occurred_at: "2026-08-31T00:00:00Z".to_owned(),
                    role: crate::memory::MemoryRole::User,
                    content: "source".to_owned(),
                    matched: true,
                    truncated: false,
                }],
            },
            preserved_source_refs: Vec::new(),
            facets: None,
        }
    }

    fn proposal(action: &str, content: &str) -> PcpHistoryRepairProposal {
        PcpHistoryRepairProposal {
            page_id: "page".to_owned(),
            expected_revision_id: "revision".to_owned(),
            action: action.to_owned(),
            reason: "bounded review".to_owned(),
            content: content.to_owned(),
            source_message_ids: vec!["message".to_owned()],
        }
    }

    #[test]
    fn transcript_ids_are_narrow_and_deduplicated() {
        let refs = vec![
            SourceRef {
                provider_id: TRANSCRIPT_PROVIDER_ID.to_owned(),
                locator: "message/two".to_owned(),
                media_type: None,
                content_digest: None,
            },
            SourceRef {
                provider_id: "web".to_owned(),
                locator: "message/ignored".to_owned(),
                media_type: None,
                content_digest: None,
            },
            SourceRef {
                provider_id: TRANSCRIPT_PROVIDER_ID.to_owned(),
                locator: "message/two".to_owned(),
                media_type: None,
                content_digest: None,
            },
            SourceRef {
                provider_id: TRANSCRIPT_PROVIDER_ID.to_owned(),
                locator: "message/one".to_owned(),
                media_type: None,
                content_digest: None,
            },
        ];
        assert_eq!(transcript_message_ids(&refs), vec!["one", "two"]);
    }

    #[test]
    fn idempotency_key_changes_with_repaired_content() {
        let entry = |content: &str| RepairLedgerEntry {
            page_id: "page".to_owned(),
            expected_revision_id: "revision".to_owned(),
            kind: "note".to_owned(),
            media_type: "text/markdown".to_owned(),
            action: "revise".to_owned(),
            reason: "restore context".to_owned(),
            content: content.to_owned(),
            source_message_ids: vec!["message".to_owned()],
            review_lane: ComputeLane::Critical.as_str().to_owned(),
            preserved_source_refs: Vec::new(),
            facets: None,
            reviewed_at: now(),
            applied_revision_id: None,
            apply_note: None,
        };
        assert_ne!(
            repair_idempotency_key(&entry("one"), RepairTask::ContentFidelity),
            repair_idempotency_key(&entry("two"), RepairTask::ContentFidelity)
        );
    }

    #[test]
    fn primary_review_can_escalate_without_mutating_content() {
        let batch = vec![review_candidate("current")];
        let validated = validate_proposals(
            &batch,
            vec![proposal("escalate", "current")],
            RepairTask::ContentFidelity,
            true,
        )
        .expect("primary review may request a critical pass");
        assert_eq!(validated[0].action, "escalate");
        let normalized = validate_proposals(
            &batch,
            vec![proposal("escalate", "changed")],
            RepairTask::ContentFidelity,
            true,
        )
        .expect("primary escalation content is locally canonicalized");
        assert_eq!(normalized[0].content, "current");
    }

    #[test]
    fn critical_review_must_return_a_final_decision() {
        let batch = vec![review_candidate("current")];
        assert!(
            validate_proposals(
                &batch,
                vec![proposal("escalate", "current")],
                RepairTask::ContentFidelity,
                false,
            )
            .is_err()
        );
        assert!(
            validate_proposals(
                &batch,
                vec![proposal("keep", "current")],
                RepairTask::ContentFidelity,
                false,
            )
            .is_ok()
        );
        assert!(
            validate_proposals(
                &batch,
                vec![proposal("revise", "changed")],
                RepairTask::ContentFidelity,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn language_repair_targets_only_english_pages_with_chinese_user_sources() {
        let mut candidate = review_candidate(
            "The current page is an English durable record whose wording should follow the user evidence language.",
        );
        candidate.review.source_messages[0].content =
            "这是用户用中文表达的长期信息，修复时应该保持原有语义，只改变表达语言。".to_owned();
        assert!(is_chinese_language_repair_candidate(&candidate));
        candidate.review.current_content = "An English concept note may preserve the proposed Chinese names “仪式社会”, “神谕社会”, and “镜像神谕社会” verbatim without making the surrounding explanation Chinese.".to_owned();
        assert!(is_chinese_language_repair_candidate(&candidate));
        candidate.review.current_content =
            "这条记录已经使用中文表达，不需要再做语言修复。".to_owned();
        assert!(!is_chinese_language_repair_candidate(&candidate));
    }

    #[test]
    fn language_repair_requires_chinese_revision_and_exact_sources() {
        let mut candidate = review_candidate(
            "The current page is an English durable record whose semantics must remain unchanged during translation.",
        );
        candidate.review.source_messages[0].content =
            "这是用户用中文表达的长期信息，需要忠实恢复成中文。".to_owned();
        let batch = vec![candidate];
        assert!(
            validate_proposals(
                &batch,
                vec![proposal("keep", &batch[0].review.current_content)],
                RepairTask::ChineseLanguageFidelity,
                false,
            )
            .is_err()
        );
        assert!(
            validate_proposals(
                &batch,
                vec![proposal(
                    "revise",
                    "这条长期记录只恢复为自然中文，同时保持全部原有语义、数字和不确定性。",
                )],
                RepairTask::ChineseLanguageFidelity,
                false,
            )
            .is_ok()
        );
    }
}
