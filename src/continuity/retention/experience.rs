//! Optional evidence at existing retention checkpoints. The reviewed account,
//! review interpretation and native publication result retain distinct meanings.
//! No new inference, transcript export, timer, or alternative Store authority.
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Result, ensure};
use pcp_client::{
    RUNTIME_EXPERIENCE_FEATURE,
    experience::{
        ExecutionReceipt, Experience, ExperienceCandidate, ReceiptOutcome, ReceiptStage,
        outbox::ExperienceOutbox,
    },
};
use pcp_core::SourceRef;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{ContinuityHost, Proposal, RetentionBasis, RetentionReview, store::QueueState};

const MAX_PENDING: usize = 64;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BoundExperience {
    identity_id: String,
    principal_id: String,
    input: ExperienceCandidate,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeliveryStatus {
    pub(super) paused: bool,
    status: String,
    reason: Option<String>,
    updated_at: String,
    last_receipt: Option<Value>,
    #[serde(default)]
    last_capture: Option<Value>,
}

impl ContinuityHost {
    fn experience_binding(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(format!(
                "{}\0{}",
                self.store.identity_id(),
                self.store.access().principal.principal_id
            ))
        )
    }

    fn experience_path(&self) -> PathBuf {
        self.retention
            .path
            .with_extension(format!("experience-{}", self.experience_binding()))
    }

    fn experience_matches(&self, item: &BoundExperience) -> bool {
        item.identity_id == self.store.identity_id()
            && item.principal_id == self.store.access().principal.principal_id
            && item.input.candidate.scope == self.pcp_scope()
    }

    pub(super) fn retention_experience_snapshot(&self, state: &QueueState) -> Value {
        let count = pending_count(&self.experience_path());
        let captured = state
            .proposals
            .values()
            .filter_map(|record| record.experience.as_ref());
        let (own, foreign): (Vec<_>, Vec<_>) =
            captured.partition(|item| self.experience_matches(item));
        json!({
            "enabled":self.experience_enabled,
            "delivery":state.experience_delivery.get(&self.experience_binding()),
            "pending":count.as_ref().ok().map(|count| count + own.len()),
            "foreignHeld":foreign.len(),
            "inspectionRequired":count.is_err() || !foreign.is_empty() || state.experience_delivery.get(&self.experience_binding()).is_some_and(|status| status.paused),
            "capacity":MAX_PENDING,
            "retryWindowDays":7,
        })
    }

    fn experience_status(
        &self,
        state: &mut QueueState,
        status: &str,
        reason: Option<String>,
        paused: bool,
    ) {
        let entry = state
            .experience_delivery
            .entry(self.experience_binding())
            .or_default();
        entry.status = status.to_owned();
        entry.reason = reason.map(|text| excerpt(&text, 300));
        entry.paused |= paused;
        entry.updated_at = super::timestamp();
    }

    pub(super) async fn capture_retention_experience(
        &self,
        state: &mut QueueState,
        id: &str,
        proposal: &Proposal,
        review: &RetentionReview,
        result: &Value,
    ) {
        if !self.experience_enabled || !eligible(review) {
            return;
        }
        // Persist the exact request together with the native receipt, before
        // any dispatch. Crash recovery never reconstructs it from live chat.
        let capture = async {
            ensure!(proposal.source_message_ids.len() <= 6 && proposal.based_on_revision_ids.len() <= 15,
                "experience source budget exceeded; full retention evidence remains in its native archive");
            let record = state.proposals.get(id).expect("retention record exists");
            let receipt_key = format!("{:x}", Sha256::digest(serde_json::to_vec(result)?));
            if record.experience_receipts.contains_key(&receipt_key) { return Ok(None); }
            ensure!(record.experience.is_none(), "previous experience is awaiting staging; preserve it before capturing a new outcome");
            let waiting = state.proposals.values().filter(|record| record.experience.is_some()).count();
            ensure!(pending_count(&self.experience_path())? + waiting < MAX_PENDING,
                "experience outbox is full; existing evidence retained and new capture declined");
            let refs = self.transcript_source_refs(&proposal.source_message_ids).await?;
            ensure!(record.experience_receipts.len() < 4,
                "native experience receipt archive is full for this proposal; existing receipts retained");
            let input = candidate(&self.retention.path, id, proposal, review, result, refs, self.pcp_scope())?;
            Ok::<_, anyhow::Error>(Some((receipt_key, BoundExperience {
                identity_id: self.store.identity_id().to_owned(),
                principal_id: self.store.access().principal.principal_id.clone(),
                input,
            })))
        }.await;
        match capture {
            Ok(None) => {}
            Ok(Some((key, item))) => {
                let record = state
                    .proposals
                    .get_mut(id)
                    .expect("retention record exists");
                record.experience_receipts.insert(key, result.clone());
                record.experience = Some(item);
                state
                    .experience_delivery
                    .entry(self.experience_binding())
                    .or_default()
                    .last_capture = Some(json!({"status":"captured", "at":super::timestamp()}));
            }
            Err(error) => {
                state
                    .experience_delivery
                    .entry(self.experience_binding())
                    .or_default()
                    .last_capture = Some(
                    json!({"status":"declined", "reason":excerpt(&error.to_string(), 300), "at":super::timestamp()}),
                );
            }
        }
    }

    pub(super) async fn retention_experience_checkpoint(&self) -> Result<()> {
        if !self.experience_enabled {
            return Ok(());
        }
        let mut state = self.retention.state.lock().await;
        if state
            .experience_delivery
            .get(&self.experience_binding())
            .is_some_and(|status| status.paused)
        {
            return Ok(());
        }
        let captured = state.proposals.iter().find_map(|(id, record)| {
            record
                .experience
                .as_ref()
                .filter(|item| self.experience_matches(item))
                .map(|item| (id.clone(), item.clone()))
        });
        let root = self.experience_path();
        if captured.is_none() && !root.exists() {
            return Ok(());
        }
        let delivery = async {
            let outbox = ExperienceOutbox::open(
                root,
                self.store.identity_id().to_owned(),
                self.store.access().principal.principal_id.clone(),
            )?;
            if let Some((id, item)) = captured {
                outbox.stage(item.input)?;
                state.proposals.get_mut(&id).unwrap().experience = None;
                // The exact request is already durable in the private outbox.
                // If this save fails, staging after restart deduplicates it.
                self.retention.save(&state).await?;
            }
            if !self
                .store
                .capabilities()
                .features
                .iter()
                .any(|feature| feature == RUNTIME_EXPERIENCE_FEATURE)
            {
                anyhow::bail!("experience Runtime capability unavailable; request retained");
            }
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                outbox.flush_one(self.store.as_ref()),
            )
            .await
            .map_err(|_| anyhow::anyhow!("experience delivery timed out; exact request retained"))?
        }
        .await;
        match delivery {
            Ok(receipt) => {
                self.experience_status(
                    &mut state,
                    if receipt.is_some() {
                        "delivered"
                    } else {
                        "idle"
                    },
                    None,
                    false,
                );
                if let Some(receipt) = receipt {
                    state
                        .experience_delivery
                        .get_mut(&self.experience_binding())
                        .unwrap()
                        .last_receipt = Some(receipt);
                }
            }
            Err(error) => {
                let retryable = retryable_delivery(&error);
                self.experience_status(
                    &mut state,
                    if retryable {
                        "pending"
                    } else {
                        "inspection_required"
                    },
                    Some(error.to_string()),
                    !retryable,
                );
            }
        }
        self.retention.save(&state).await
    }
}

fn eligible(review: &RetentionReview) -> bool {
    matches!(
        review.retention_basis,
        Some(
            RetentionBasis::ConcreteEvidence
                | RetentionBasis::ConsequentialEvent
                | RetentionBasis::MeaningfulRecurrence
        )
    ) && review
        .recall_value
        .as_deref()
        .is_some_and(|value| value.trim().chars().count() >= 8)
}

fn candidate(
    archive: &std::path::Path,
    id: &str,
    proposal: &Proposal,
    review: &RetentionReview,
    result: &Value,
    sources: Vec<SourceRef>,
    scope: &str,
) -> Result<ExperienceCandidate> {
    let receipt_key = format!("{:x}", Sha256::digest(serde_json::to_vec(result)?));
    let written = result["status"] == "written";
    let deferred = result["status"] == "deferred";
    ensure!(
        written || deferred,
        "experience requires a native reviewed retention outcome"
    );
    let experience = Experience {
        topic_key: Some(id.to_owned()),
        conditions: Some(excerpt(&format!("Native retention review; attribution={:?}; receipt source observed at={}. Future recall value: {}", review.attribution, result["observedAt"].as_str().unwrap_or("not recorded"), review.recall_value.as_deref().unwrap_or_default()), 600)),
        attempt: excerpt(&format!("Review this source-backed account for retention (excerpt): {}", proposal.content), 600),
        observation: if written {
            format!("PCP ingest returned a publication receipt for Revision {} (created={}). This establishes retention only; it does not independently validate the account or establish task success.", result["revisionId"].as_str().unwrap_or_default(), result["created"])
        } else {
            format!("Retention remained deferred after review: {}", excerpt(&review.rationale, 800))
        },
        interpretation: Some(excerpt(&review.rationale, 600)),
        unresolved: if deferred { vec![excerpt(&format!("Retention is unresolved: {}", review.rationale), 200)] } else { vec![] },
        receipts: vec![ExecutionReceipt {
            source: SourceRef {
                provider_id: "symbiont-retention".to_owned(),
                locator: format!("{}#/proposals/{id}/experienceReceipts/{receipt_key}", archive.display()),
                media_type: Some("application/json".to_owned()),
                content_digest: Some(format!("sha256:{receipt_key}")),
            },
            stage: if written { ReceiptStage::Publication } else { ReceiptStage::Validation },
            outcome: if written { ReceiptOutcome::Succeeded } else { ReceiptOutcome::Unknown },
            summary: if written { "Native PCP retention publication completed. This receipt does not verify task success." } else { "Native retention review deferred publication; no task-success conclusion." }.to_owned(),
            version: result["revisionId"].as_str().map(str::to_owned),
        }],
    };
    let mut input = experience.into_candidate(
        scope.to_owned(),
        "Source-backed retention experience".to_owned(),
    )?;
    input.candidate.source_refs.extend(sources);
    input.candidate.based_on_revision_ids = proposal.based_on_revision_ids.clone();
    input
        .candidate
        .based_on_revision_ids
        .extend(review.related_revision_ids.clone());
    if let Some(revision) = result["revisionId"].as_str() {
        input
            .candidate
            .based_on_revision_ids
            .push(revision.to_owned());
    }
    input.candidate.based_on_revision_ids.sort();
    input.candidate.based_on_revision_ids.dedup();
    ensure!(
        input.candidate.based_on_revision_ids.len() <= 16,
        "experience Revision budget exceeded; retain the full native review instead of dropping sources"
    );
    // Bind the final source set too, after the SDK's structured conversion.
    input.candidate.event_id.clear();
    input.candidate.event_id = format!(
        "experience:{:x}",
        Sha256::digest(serde_json::to_vec(&input)?)
    );
    Ok(input)
}

fn pending_count(root: &std::path::Path) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let entries = std::fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    Ok(entries
        .iter()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count())
}

fn excerpt(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        format!("{}…", value.chars().take(limit - 1).collect::<String>())
    }
}

fn retryable_delivery(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::UnexpectedEof
            )
        })
    }) || error.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("closed before responding")
            || text.contains("closed without a response")
            || text.contains("timed out")
            || text.contains("candidate receipt was not confirmed")
            || text.contains("experience Runtime capability unavailable")
    })
}
