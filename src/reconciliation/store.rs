use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use super::{ReconciliationAction, ReconciliationMode, ReconciliationProposal, ReconciliationRun};

const MAX_RECENT_RUNS: usize = 30;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconciliationState {
    latest_preview_id: Option<String>,
    runs: Vec<ReconciliationRun>,
}

pub struct ReconciliationStore {
    path: PathBuf,
    state: RwLock<ReconciliationState>,
}

pub struct CompletedRun {
    pub summary: Option<String>,
    pub proposals: Vec<ReconciliationProposal>,
    pub actions: Vec<ReconciliationAction>,
    pub trace_id: Option<String>,
    pub model: Option<String>,
    pub total_tokens: u64,
}

impl ReconciliationStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let mut state = match fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str::<ReconciliationState>(&content)
                .with_context(|| format!("parse Reconciliation state {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => ReconciliationState::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read Reconciliation state {}", path.display()));
            }
        };
        let interrupted_at = now();
        for run in &mut state.runs {
            if run.status == "running" {
                run.status = "interrupted".to_owned();
                run.completed_at = Some(interrupted_at.clone());
                run.error = Some("service_restarted".to_owned());
            }
        }
        let store = Self {
            path,
            state: RwLock::new(state),
        };
        store.persist().await?;
        Ok(store)
    }

    pub async fn start_run(
        &self,
        mode: ReconciliationMode,
        trigger: &str,
        inventory_digest: String,
        candidate_count: usize,
        preview_run_id: Option<String>,
    ) -> Result<String> {
        let id = format!(
            "rec_{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let run = ReconciliationRun {
            id: id.clone(),
            mode,
            trigger: trigger.to_owned(),
            status: "running".to_owned(),
            started_at: now(),
            completed_at: None,
            inventory_digest,
            candidate_count,
            preview_run_id,
            summary: None,
            proposals: Vec::new(),
            actions: Vec::new(),
            trace_id: None,
            model: None,
            total_tokens: 0,
            error: None,
        };
        let mut state = self.state.write().await;
        state.runs.insert(0, run);
        state.runs.truncate(MAX_RECENT_RUNS);
        drop(state);
        self.persist().await?;
        Ok(id)
    }

    pub async fn complete_run(&self, run_id: &str, completed: CompletedRun) -> Result<()> {
        let mut state = self.state.write().await;
        let run = state
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown Reconciliation run {run_id}"))?;
        run.status = "completed".to_owned();
        run.completed_at = Some(now());
        run.summary = completed.summary;
        run.proposals = completed.proposals;
        run.actions = completed.actions;
        run.trace_id = completed.trace_id;
        run.model = completed.model;
        run.total_tokens = completed.total_tokens;
        run.error = None;
        if run.mode == ReconciliationMode::Preview {
            state.latest_preview_id = Some(run_id.to_owned());
        }
        drop(state);
        self.persist().await
    }

    pub async fn fail_run(&self, run_id: &str, error: &str) -> Result<()> {
        let mut state = self.state.write().await;
        let run = state
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown Reconciliation run {run_id}"))?;
        run.status = "error".to_owned();
        run.completed_at = Some(now());
        run.error = Some(error.to_owned());
        drop(state);
        self.persist().await
    }

    pub async fn interrupt_run(&self, run_id: &str) -> Result<()> {
        let mut state = self.state.write().await;
        let run = state
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown Reconciliation run {run_id}"))?;
        run.status = "interrupted".to_owned();
        run.completed_at = Some(now());
        run.error = Some("superseded_by_user_input".to_owned());
        drop(state);
        self.persist().await
    }

    pub async fn latest_preview(&self) -> Option<ReconciliationRun> {
        let state = self.state.read().await;
        let id = state.latest_preview_id.as_deref()?;
        state.runs.iter().find(|run| run.id == id).cloned()
    }

    pub async fn run(&self, run_id: &str) -> Option<ReconciliationRun> {
        self.state
            .read()
            .await
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
    }

    pub async fn recent_runs(&self) -> Vec<ReconciliationRun> {
        self.state.read().await.runs.clone()
    }

    pub async fn has_completed_apply(&self, preview_run_id: &str) -> bool {
        self.state.read().await.runs.iter().any(|run| {
            run.mode == ReconciliationMode::Apply
                && run.status == "completed"
                && run.preview_run_id.as_deref() == Some(preview_run_id)
        })
    }

    async fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create Reconciliation directory {}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(&*self.state.read().await)
            .context("encode Reconciliation state")?;
        let temporary = self.path.with_extension("tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write Reconciliation state {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace Reconciliation state {}", self.path.display()))
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
