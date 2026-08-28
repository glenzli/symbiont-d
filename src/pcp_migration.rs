//! One-time, resumable migration from the local transcript into a fresh PCP Store.
//!
//! The transcript remains authoritative. Each bounded batch is shown to the model so Symbiont,
//! rather than a mechanical importer, decides what has durable value. The watermark is keyed by
//! PCP identity; retrying a failed batch is safe because PCP writes use deterministic event IDs.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use tracing::info;

use crate::{
    codex::{CodexClient, PcpTranscriptMigrationRequest},
    compute::ComputeStore,
    continuity::ContinuityHost,
    profile::ProfileStore,
    transcript::TranscriptStore,
};

const MIGRATION_BATCH_MESSAGES: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct PcpTranscriptMigrationReport {
    pub pcp_identity_id: String,
    pub through_sequence: i64,
    pub batches_completed: usize,
    pub messages_assessed: usize,
    pub records_written: usize,
}

pub async fn migrate_transcript(
    codex: &mut CodexClient,
    transcript: Arc<TranscriptStore>,
    continuity: Arc<ContinuityHost>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
) -> Result<PcpTranscriptMigrationReport> {
    let pcp_identity_id = continuity.pcp_identity_id().to_owned();
    let through_sequence = transcript.max_visible_sequence().await?;
    let mut watermark = transcript.pcp_migration_watermark(&pcp_identity_id).await?;
    let mut report = PcpTranscriptMigrationReport {
        pcp_identity_id: pcp_identity_id.clone(),
        through_sequence,
        ..PcpTranscriptMigrationReport::default()
    };
    while watermark < through_sequence {
        let batch = transcript
            .pcp_migration_batch(watermark, through_sequence, MIGRATION_BATCH_MESSAGES)
            .await?;
        if batch.is_empty() {
            transcript
                .advance_pcp_migration_watermark(&pcp_identity_id, through_sequence)
                .await?;
            break;
        }
        let batch_end = batch
            .last()
            .map(|message| message.sequence)
            .context("transcript migration batch lost its final sequence")?;
        let batch_bundle = serde_json::to_string_pretty(&batch)?;
        let compute_snapshot = compute.snapshot().await;
        let profile_snapshot = profile.snapshot().await;
        let continuity_context = continuity.context_seed(None).await;
        let (_input_tx, input_events) = watch::channel(0_u64);
        let (event_tx, mut event_rx) = mpsc::channel(256);
        let event_drain = tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
        let outcome = codex
            .migrate_transcript_batch(PcpTranscriptMigrationRequest {
                batch_bundle: &batch_bundle,
                compute: &compute_snapshot,
                profile: &profile_snapshot,
                continuity_context: &continuity_context,
                input_events,
                events: event_tx,
            })
            .await
            .with_context(|| {
                format!(
                    "judge PCP transcript migration batch after sequence {watermark} through {batch_end}"
                )
            })?;
        event_drain.await.context("join migration event drain")?;
        transcript
            .advance_pcp_migration_watermark(&pcp_identity_id, batch_end)
            .await?;
        watermark = batch_end;
        report.batches_completed += 1;
        report.messages_assessed += batch.len();
        report.records_written += outcome.records_written;
        info!(
            pcp_identity_id,
            completed_sequence = watermark,
            messages_assessed = batch.len(),
            records_written = outcome.records_written,
            "completed one model-judged PCP transcript migration batch"
        );
    }
    Ok(report)
}
