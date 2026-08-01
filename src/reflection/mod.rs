mod store;
mod worker;

use serde::{Deserialize, Serialize};

pub use store::ReflectionStore;
pub use worker::ReflectionHandle;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionConfig {
    pub enabled: bool,
    pub settle_seconds: u32,
    pub retention_days: u32,
    pub capture_read_state: bool,
    pub follow_ups_enabled: bool,
    #[serde(default = "enabled_by_default")]
    pub continuations_enabled: bool,
    #[serde(default = "enabled_by_default")]
    pub proactive_messages_enabled: bool,
    pub daily_token_limit: u64,
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            settle_seconds: 20,
            retention_days: 30,
            capture_read_state: true,
            follow_ups_enabled: true,
            continuations_enabled: true,
            proactive_messages_enabled: true,
            daily_token_limit: 1_000_000,
        }
    }
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeState {
    Forming,
    Active,
    Dormant,
    Closed,
}

impl EpisodeState {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "forming" => Some(Self::Forming),
            "active" => Some(Self::Active),
            "dormant" => Some(Self::Dormant),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forming => "forming",
            Self::Active => "active",
            Self::Dormant => "dormant",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    Tentative,
    Working,
    Contradicted,
    Superseded,
}

impl HypothesisStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tentative" => Some(Self::Tentative),
            "working" => Some(Self::Working),
            "contradicted" => Some(Self::Contradicted),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tentative => "tentative",
            Self::Working => "working",
            Self::Contradicted => "contradicted",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisHorizon {
    Momentary,
    Current,
    StableCandidate,
}

impl HypothesisHorizon {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "momentary" => Some(Self::Momentary),
            "current" => Some(Self::Current),
            "stable_candidate" => Some(Self::StableCandidate),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Momentary => "momentary",
            Self::Current => "current",
            Self::StableCandidate => "stable_candidate",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionEvent {
    pub id: i64,
    pub kind: String,
    pub occurred_at: String,
    pub revision_id: Option<String>,
    pub related_revision_id: Option<String>,
    pub role: Option<String>,
    pub content_chars: u64,
    pub payload: serde_json::Value,
    pub retracted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HunchFeedbackTarget {
    pub page_id: String,
    pub revision_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationEpisode {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub state: EpisodeState,
    pub started_at: String,
    pub last_activity_at: String,
    pub updated_at: String,
    pub source_revision_ids: Vec<String>,
    pub parent_episode_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingHypothesis {
    pub id: String,
    pub statement: String,
    pub evidence: String,
    pub alternatives: String,
    pub status: HypothesisStatus,
    pub horizon: HypothesisHorizon,
    pub revisit_after: Option<String>,
    pub updated_at: String,
    pub source_revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredFollowUp {
    pub id: String,
    pub reason: String,
    pub not_before: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub triggered_at: Option<String>,
    pub completed_at: Option<String>,
    pub outcome: Option<String>,
    pub source_revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionRun {
    pub id: String,
    pub trigger: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub from_event_id: Option<i64>,
    pub to_event_id: Option<i64>,
    pub event_count: u64,
    pub summary: Option<String>,
    pub trace_id: Option<String>,
    pub model: Option<String>,
    pub total_tokens: u64,
    pub error: Option<String>,
    pub actions: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflectionPhase {
    Disabled,
    NeedsSetup,
    Waiting,
    Reflecting,
    TokenLimit,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionRuntime {
    pub phase: ReflectionPhase,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_summary: Option<String>,
    pub last_error: Option<String>,
    pub pending_events: u64,
    pub current_activity: Option<String>,
}

impl Default for ReflectionRuntime {
    fn default() -> Self {
        Self {
            phase: ReflectionPhase::Waiting,
            next_run_at: None,
            last_run_at: None,
            last_summary: None,
            last_error: None,
            pending_events: 0,
            current_activity: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionSnapshot {
    pub config: ReflectionConfig,
    pub runtime: ReflectionRuntime,
    pub episodes: Vec<ConversationEpisode>,
    pub hypotheses: Vec<WorkingHypothesis>,
    pub follow_ups: Vec<DeferredFollowUp>,
    pub recent_runs: Vec<ReflectionRun>,
}

#[derive(Clone, Debug)]
pub struct EpisodeInput {
    pub id: Option<String>,
    pub title: String,
    pub summary: String,
    pub state: EpisodeState,
    pub source_revision_ids: Vec<String>,
    pub parent_episode_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct HypothesisInput {
    pub id: Option<String>,
    pub statement: String,
    pub evidence: String,
    pub alternatives: String,
    pub status: HypothesisStatus,
    pub horizon: HypothesisHorizon,
    pub revisit_after: Option<String>,
    pub source_revision_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FollowUpInput {
    pub reason: String,
    pub not_before: String,
    pub source_revision_ids: Vec<String>,
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
