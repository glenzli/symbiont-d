mod store;
mod worker;

use serde::{Deserialize, Serialize};

pub use store::ReconciliationStore;
pub use worker::{ReconciliationDependencies, ReconciliationHandle};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationMode {
    Preview,
    Apply,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationProposalKind {
    Classify,
    Synthesize,
    Link,
    AssessValidity,
    Resummarize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationProposal {
    pub action: ReconciliationProposalKind,
    pub subject: String,
    pub reason: String,
    #[serde(alias = "revision_ids")]
    pub revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationAction {
    pub tool: String,
    pub target_revision_ids: Vec<String>,
    pub result_page_id: Option<String>,
    pub result_revision_id: Option<String>,
    pub result_relation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationRun {
    pub id: String,
    pub mode: ReconciliationMode,
    pub trigger: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub inventory_digest: String,
    pub candidate_count: usize,
    pub preview_run_id: Option<String>,
    pub summary: Option<String>,
    pub proposals: Vec<ReconciliationProposal>,
    pub actions: Vec<ReconciliationAction>,
    pub trace_id: Option<String>,
    pub model: Option<String>,
    pub total_tokens: u64,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationPhase {
    Idle,
    Previewing,
    Applying,
    NeedsSetup,
    TokenLimit,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationRuntime {
    pub phase: ReconciliationPhase,
    pub candidate_count: usize,
    pub current_activity: Option<String>,
    pub last_run_at: Option<String>,
    pub last_summary: Option<String>,
    pub last_error: Option<String>,
}

impl Default for ReconciliationRuntime {
    fn default() -> Self {
        Self {
            phase: ReconciliationPhase::Idle,
            candidate_count: 0,
            current_activity: None,
            last_run_at: None,
            last_summary: None,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationSnapshot {
    pub runtime: ReconciliationRuntime,
    pub latest_preview: Option<ReconciliationRun>,
    pub recent_runs: Vec<ReconciliationRun>,
}

pub struct ReconciliationModelOutcome {
    pub invocations: Vec<crate::usage::InvocationRecord>,
    pub summary: Option<String>,
    pub proposals: Vec<ReconciliationProposal>,
    pub actions: Vec<ReconciliationAction>,
    pub interrupted: bool,
}
