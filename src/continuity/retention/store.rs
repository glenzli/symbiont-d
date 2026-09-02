//! Local, restart-safe proposals and write receipts; never authoritative PCP data.
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{fs, sync::Mutex};

use super::{Proposal, ReviewSnapshot};

pub(in crate::continuity) struct RetentionQueue {
    path: PathBuf,
    pub(super) state: Mutex<QueueState>,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct QueueState {
    #[serde(default)]
    pub(super) proposals: BTreeMap<String, Record>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Record {
    pub(super) proposal: Proposal,
    pub(super) status: String,
    pub(super) proposed_at: String,
    pub(super) retry_after: String,
    pub(super) reason: String,
    pub(super) review: Option<ReviewSnapshot>,
    pub(super) result: Option<Value>,
}

impl Record {
    pub(super) fn new(proposal: Proposal) -> Self {
        Self {
            proposal,
            status: "pending".to_owned(),
            proposed_at: super::timestamp(),
            retry_after: super::timestamp(),
            reason: "awaiting_preflight".to_owned(),
            review: None,
            result: None,
        }
    }

    pub(super) fn defer(&mut self, reason: impl Into<String>) {
        self.status = "pending".to_owned();
        self.reason = reason.into();
        self.retry_after = (Utc::now() + Duration::minutes(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.review = None;
    }
}

impl RetentionQueue {
    pub(in crate::continuity) fn path_for(transcript: &Path, identity_id: &str) -> PathBuf {
        // A receipt from another PCP Store must never count as a write here,
        // nor should switching Stores silently replay that Store's proposals.
        let identity = format!("{:x}", Sha256::digest(identity_id.as_bytes()));
        transcript.with_extension(format!("retention-{identity}.json"))
    }

    pub(in crate::continuity) async fn open(path: PathBuf) -> Result<Self> {
        let state = match fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).context("read retention proposal queue")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => QueueState::default(),
            Err(error) => return Err(error).context("open retention proposal queue"),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    // The caller holds state across preflight and commit. Foreground and
    // Reflection therefore observe each other's receipts, including index lag.
    pub(super) async fn save(&self, state: &QueueState) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(state)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let parent = path.parent().context("retention queue has no parent")?;
            std::fs::create_dir_all(parent)?;
            // Unique, owner-only temporary file; never expose raw chat evidence
            // through a world-readable or partially written queue.
            let mut pending = tempfile::NamedTempFile::new_in(parent)?;
            pending.write_all(&bytes)?;
            pending.as_file().sync_all()?;
            pending.persist(&path).context("commit retention queue")?;
            Ok(())
        })
        .await
        .context("save retention queue task")?
    }
}
