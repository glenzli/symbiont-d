use std::sync::Arc;

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::{continuity::ContinuityHost, reflection::ReflectionStore};

/// Retained for the status API. PCP v0.8 separates tenant source ingestion
/// from Runtime-owned semantic projections.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PcpIndexPhase {
    #[default]
    Idle,
    Disabled,
    Error,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcpIndexSnapshot {
    pub phase: PcpIndexPhase,
    pub episode_pages: u64,
    pub created_pages: u64,
    pub revised_pages: u64,
    pub unchanged_pages: u64,
    pub skipped_episode_pages: u64,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
}

pub struct PcpIndex {
    _continuity: Arc<ContinuityHost>,
    _reflection: Arc<ReflectionStore>,
    snapshot: RwLock<PcpIndexSnapshot>,
}

impl PcpIndex {
    pub fn new(continuity: Arc<ContinuityHost>, reflection: Arc<ReflectionStore>) -> Self {
        Self {
            _continuity: continuity,
            _reflection: reflection,
            snapshot: RwLock::new(disabled_snapshot()),
        }
    }

    pub async fn snapshot(&self) -> PcpIndexSnapshot {
        self.snapshot.read().await.clone()
    }

    /// Compatibility endpoint for existing UI callers. It never writes,
    /// inspects historical episodes, or invokes a model.
    pub async fn sync_all(&self) -> Result<PcpIndexSnapshot> {
        let snapshot = disabled_snapshot();
        *self.snapshot.write().await = snapshot.clone();
        Ok(snapshot)
    }
}

fn disabled_snapshot() -> PcpIndexSnapshot {
    PcpIndexSnapshot {
        phase: PcpIndexPhase::Disabled,
        last_sync_at: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
        last_error: Some(
            "PCP v0.8 tenant mode does not build Symbiont semantic indexes".to_owned(),
        ),
        ..PcpIndexSnapshot::default()
    }
}
