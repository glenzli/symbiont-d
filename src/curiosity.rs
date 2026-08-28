use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use pcp_core::WriteResult;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::{
    continuity::ContinuityHost,
    symbiont_state::{LocalContextDocument, SymbiontStateStore},
};

const HUNCH_KIND: &str = "symbiont_hunch";
const MAX_HUNCH_FIELD_CHARS: usize = 4_000;
const MAX_HUNCHES: u32 = 100;
const MAX_ACTIVE_PROMPT_HUNCHES: usize = 12;
const MAX_CLOSED_PROMPT_HUNCHES: usize = 4;
const MAX_EXPLORATION_HUNCHES: usize = 6;
const MAX_EXPLORATION_CONSTRAINTS: usize = 8;
const MAX_PROMPT_FIELD_CHARS: usize = 800;
const FEEDBACK_COOLDOWN_HOURS: i64 = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HunchOrigin {
    User,
    Symbiont,
    External,
}

impl HunchOrigin {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "symbiont" => Some(Self::Symbiont),
            "external" => Some(Self::External),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Symbiont => "symbiont",
            Self::External => "external",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HunchState {
    Germinating,
    Watching,
    Dormant,
    Resolved,
}

impl HunchState {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "germinating" => Some(Self::Germinating),
            "watching" => Some(Self::Watching),
            "dormant" => Some(Self::Dormant),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Germinating => "germinating",
            Self::Watching => "watching",
            Self::Dormant => "dormant",
            Self::Resolved => "resolved",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Germinating | Self::Watching)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HunchAttention {
    Ready,
    AwaitingUser,
    FeedbackPending,
    Cooldown,
}

impl HunchAttention {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ready" => Some(Self::Ready),
            "awaiting_user" => Some(Self::AwaitingUser),
            "feedback_pending" => Some(Self::FeedbackPending),
            "cooldown" => Some(Self::Cooldown),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::AwaitingUser => "awaiting_user",
            Self::FeedbackPending => "feedback_pending",
            Self::Cooldown => "cooldown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewHunch {
    pub question: String,
    pub origin: HunchOrigin,
    pub why_alive: String,
    pub what_would_change_it: String,
    pub source_revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct HunchPatch {
    pub question: Option<String>,
    pub why_alive: Option<String>,
    pub what_would_change_it: Option<String>,
    pub state: Option<HunchState>,
    pub resolution: Option<String>,
    pub source_revision_ids: Vec<String>,
    pub attention: Option<HunchAttention>,
    pub last_surfaced_message_revision_id: Option<String>,
    pub last_feedback_revision_id: Option<String>,
    pub eligible_after: Option<String>,
    pub feedback_assessment: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunch {
    pub page_id: String,
    pub revision_id: String,
    pub question: String,
    pub origin: HunchOrigin,
    pub why_alive: String,
    pub what_would_change_it: String,
    pub state: HunchState,
    pub resolution: Option<String>,
    pub source_revision_ids: Vec<String>,
    pub last_explored_at: Option<String>,
    pub attention: HunchAttention,
    pub last_surfaced_message_revision_id: Option<String>,
    pub last_feedback_revision_id: Option<String>,
    pub eligible_after: Option<String>,
    pub feedback_assessment: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CuriositySnapshot {
    pub hunches: Vec<Hunch>,
    pub active_count: usize,
}

#[derive(Clone)]
pub struct CuriosityStore {
    continuity: Arc<ContinuityHost>,
    state: Arc<SymbiontStateStore>,
}

impl CuriosityStore {
    pub fn from_state(continuity: Arc<ContinuityHost>, state: Arc<SymbiontStateStore>) -> Self {
        Self { continuity, state }
    }

    #[cfg(test)]
    pub fn new(continuity: Arc<ContinuityHost>) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-curiosity-state-{nonce}.sqlite3"));
        Self::from_state(continuity, Arc::new(SymbiontStateStore::for_test(path)))
    }

    pub async fn snapshot(&self) -> Result<CuriositySnapshot> {
        let hunches = self
            .state
            .list_context_kind(HUNCH_KIND, MAX_HUNCHES as usize)
            .await?
            .into_iter()
            .filter_map(|document| Hunch::from_document(&document))
            .collect::<Result<Vec<_>>>()?;
        let active_count = hunches
            .iter()
            .filter(|hunch| hunch.state.is_active())
            .count();
        Ok(CuriositySnapshot {
            hunches,
            active_count,
        })
    }

    pub async fn open(&self, draft: NewHunch) -> Result<WriteResult> {
        validate_field("question", &draft.question)?;
        validate_field("why_alive", &draft.why_alive)?;
        validate_field("what_would_change_it", &draft.what_would_change_it)?;
        let sources = normalize_sources(draft.source_revision_ids);
        let state = HunchState::Germinating;
        let facets = hunch_facets(HunchFacetValues {
            question: &draft.question,
            origin: draft.origin,
            why_alive: &draft.why_alive,
            what_would_change_it: &draft.what_would_change_it,
            state,
            resolution: None,
            last_explored_at: None,
            attention: HunchAttention::Ready,
            last_surfaced_message_revision_id: None,
            last_feedback_revision_id: None,
            eligible_after: None,
            feedback_assessment: None,
        });
        self.continuity.verify_context_source_ids(&sources).await?;
        let document = self
            .state
            .create_context(
                HUNCH_KIND,
                &hunch_markdown(
                    &draft.question,
                    &draft.why_alive,
                    &draft.what_would_change_it,
                    state,
                    None,
                ),
                sources,
                Some(facets),
            )
            .await
            .context("write local Hunch")?;
        Ok(WriteResult {
            page_id: document.document_id,
            revision_id: document.revision_id,
            created: true,
        })
    }

    pub async fn revise(
        &self,
        page_id: &str,
        expected_revision_id: &str,
        patch: HunchPatch,
        explored: bool,
    ) -> Result<WriteResult> {
        let current = self
            .read_revision(expected_revision_id)
            .await?
            .context("Hunch Revision was not found")?;
        if current.page_id != page_id {
            anyhow::bail!("Hunch Page and expected Revision do not match");
        }

        let question = patch.question.unwrap_or(current.question);
        let why_alive = patch.why_alive.unwrap_or(current.why_alive);
        let what_would_change_it = patch
            .what_would_change_it
            .unwrap_or(current.what_would_change_it);
        let state = patch.state.unwrap_or(current.state);
        let resolution = patch.resolution.or(current.resolution);
        validate_field("question", &question)?;
        validate_field("why_alive", &why_alive)?;
        validate_field("what_would_change_it", &what_would_change_it)?;
        if let Some(value) = resolution.as_deref() {
            validate_field("resolution", value)?;
        }
        let last_explored_at = if explored {
            Some(now())
        } else {
            current.last_explored_at
        };
        let attention = patch.attention.unwrap_or(current.attention);
        let last_surfaced_message_revision_id = patch
            .last_surfaced_message_revision_id
            .or(current.last_surfaced_message_revision_id);
        let last_feedback_revision_id = patch
            .last_feedback_revision_id
            .or(current.last_feedback_revision_id);
        let eligible_after = patch.eligible_after.or(current.eligible_after);
        let feedback_assessment = patch.feedback_assessment.or(current.feedback_assessment);
        let sources = normalize_sources(patch.source_revision_ids);
        self.continuity.verify_context_source_ids(&sources).await?;
        let document = self
            .state
            .revise_context(
                page_id,
                expected_revision_id,
                HUNCH_KIND,
                &hunch_markdown(
                    &question,
                    &why_alive,
                    &what_would_change_it,
                    state,
                    resolution.as_deref(),
                ),
                sources,
                Some(hunch_facets(HunchFacetValues {
                    question: &question,
                    origin: current.origin,
                    why_alive: &why_alive,
                    what_would_change_it: &what_would_change_it,
                    state,
                    resolution: resolution.as_deref(),
                    last_explored_at: last_explored_at.as_deref(),
                    attention,
                    last_surfaced_message_revision_id: last_surfaced_message_revision_id.as_deref(),
                    last_feedback_revision_id: last_feedback_revision_id.as_deref(),
                    eligible_after: eligible_after.as_deref(),
                    feedback_assessment: feedback_assessment.as_deref(),
                })),
            )
            .await
            .context("revise local Hunch")?;
        Ok(WriteResult {
            page_id: document.document_id,
            revision_id: document.revision_id,
            created: false,
        })
    }

    pub async fn retire(
        &self,
        page_id: &str,
        expected_revision_id: &str,
        state: HunchState,
        resolution: Option<String>,
        source_revision_ids: Vec<String>,
        explored: bool,
    ) -> Result<WriteResult> {
        if !matches!(state, HunchState::Dormant | HunchState::Resolved) {
            anyhow::bail!("retired Hunch state must be dormant or resolved");
        }
        self.revise(
            page_id,
            expected_revision_id,
            HunchPatch {
                state: Some(state),
                resolution,
                source_revision_ids,
                ..HunchPatch::default()
            },
            explored,
        )
        .await
    }

    pub async fn mark_surfaced(
        &self,
        page_id: &str,
        expected_revision_id: &str,
        message_revision_id: &str,
    ) -> Result<Option<WriteResult>> {
        let Some(current) = self.read_revision(expected_revision_id).await? else {
            return Ok(None);
        };
        if current.page_id != page_id || !current.state.is_active() {
            return Ok(None);
        }
        self.revise(
            page_id,
            expected_revision_id,
            HunchPatch {
                attention: Some(HunchAttention::AwaitingUser),
                last_surfaced_message_revision_id: Some(message_revision_id.to_owned()),
                eligible_after: Some(feedback_cooldown_at()),
                source_revision_ids: vec![message_revision_id.to_owned()],
                ..HunchPatch::default()
            },
            false,
        )
        .await
        .map(Some)
    }

    pub async fn mark_feedback_pending(
        &self,
        surfaced_revision_id: &str,
        feedback_revision_id: &str,
    ) -> Result<Option<WriteResult>> {
        let Some(surfaced) = self.read_revision(surfaced_revision_id).await? else {
            return Ok(None);
        };
        let Some(current) = self.read_document(&surfaced.page_id).await? else {
            return Ok(None);
        };
        if !current.state.is_active() {
            return Ok(None);
        }
        self.revise(
            &current.page_id,
            &current.revision_id,
            HunchPatch {
                attention: Some(HunchAttention::FeedbackPending),
                last_feedback_revision_id: Some(feedback_revision_id.to_owned()),
                source_revision_ids: vec![feedback_revision_id.to_owned()],
                ..HunchPatch::default()
            },
            false,
        )
        .await
        .map(Some)
    }

    pub async fn acknowledge_feedback(
        &self,
        page_id: &str,
        expected_revision_id: &str,
        feedback_revision_id: &str,
        assessment: &str,
    ) -> Result<WriteResult> {
        validate_field("feedback assessment", assessment)?;
        self.revise(
            page_id,
            expected_revision_id,
            HunchPatch {
                attention: Some(HunchAttention::Cooldown),
                last_feedback_revision_id: Some(feedback_revision_id.to_owned()),
                eligible_after: Some(feedback_cooldown_at()),
                feedback_assessment: Some(assessment.to_owned()),
                source_revision_ids: vec![feedback_revision_id.to_owned()],
                ..HunchPatch::default()
            },
            false,
        )
        .await
    }

    pub async fn pending_feedback_page_ids(&self, page_ids: &[String]) -> Result<Vec<String>> {
        let mut pending = Vec::new();
        for page_id in page_ids {
            let Some(current) = self.read_document(page_id).await? else {
                continue;
            };
            if current.state.is_active() && current.attention == HunchAttention::FeedbackPending {
                pending.push(page_id.clone());
            }
        }
        pending.sort();
        pending.dedup();
        Ok(pending)
    }

    pub async fn prompt(&self) -> Result<String> {
        let snapshot = self.snapshot().await?;
        let mut active = snapshot
            .hunches
            .iter()
            .filter(|hunch| hunch.state.is_active())
            .take(MAX_ACTIVE_PROMPT_HUNCHES)
            .map(prompt_hunch)
            .collect::<Vec<_>>();
        let closed = snapshot
            .hunches
            .iter()
            .filter(|hunch| !hunch.state.is_active())
            .take(MAX_CLOSED_PROMPT_HUNCHES)
            .map(|hunch| {
                format!(
                    "- `{}` [{}]: {}",
                    hunch.page_id,
                    hunch.state.as_str(),
                    truncate(&hunch.question, MAX_PROMPT_FIELD_CHARS)
                )
            })
            .collect::<Vec<_>>();

        if active.is_empty() {
            active.push("- No active Hunches yet.".to_owned());
        }
        let mut prompt = format!(
            "Curiosity Map is symbiont-d's own revisable set of questions, not the user's profile, \
             preferences, or instructions. Use it to continue investigations across model and \
             thread changes. Do not create a Hunch for every topic; revise an existing one when \
             the underlying question is the same. `feedback_pending` must be reconciled before \
             further exploration. For `awaiting_user` or `cooldown`, do not repeat the same \
             investigation before `eligible after` merely because the user was silent; only \
             materially new or urgent evidence justifies an exception.\n\n{}",
            active.join("\n\n")
        );
        if !closed.is_empty() {
            prompt.push_str(
                "\n\nRecently dormant or resolved; do not reopen without new evidence:\n",
            );
            prompt.push_str(&closed.join("\n"));
        }
        Ok(prompt)
    }

    pub async fn exploration_prompt(&self) -> Result<String> {
        let snapshot = self.snapshot().await?;
        let ready = snapshot
            .hunches
            .iter()
            .filter(|hunch| hunch.state.is_active() && hunch.attention == HunchAttention::Ready)
            .take(MAX_EXPLORATION_HUNCHES)
            .map(prompt_hunch)
            .collect::<Vec<_>>();
        let constrained = snapshot
            .hunches
            .iter()
            .filter(|hunch| hunch.state.is_active() && hunch.attention != HunchAttention::Ready)
            .take(MAX_EXPLORATION_CONSTRAINTS)
            .map(|hunch| {
                format!(
                    "- revision `{}` [{} until {}]: {}",
                    hunch.revision_id,
                    hunch.attention.as_str(),
                    hunch.eligible_after.as_deref().unwrap_or("state changes"),
                    truncate(&hunch.question, 240)
                )
            })
            .collect::<Vec<_>>();

        let mut prompt = vec![
            "Curiosity Map contributes only Symbiont-owned investigation candidates. These are optional questions, not user priorities or a requirement to stay on known themes. A credible external candidate may be unrelated. Open Loops are supplied separately and should not be copied into new Hunches.".to_owned(),
        ];
        if ready.is_empty() {
            prompt.push("No Hunch is currently ready for exploration.".to_owned());
        } else {
            prompt.push("<ready-investigation-candidates>".to_owned());
            prompt.push(ready.join("\n\n"));
            prompt.push("</ready-investigation-candidates>".to_owned());
        }
        if !constrained.is_empty() {
            prompt.push(
                "<excluded-investigations>Do not select these merely because they appear here."
                    .to_owned(),
            );
            prompt.push(constrained.join("\n"));
            prompt.push("</excluded-investigations>".to_owned());
        }
        Ok(prompt.join("\n\n"))
    }

    async fn read_revision(&self, revision_id: &str) -> Result<Option<Hunch>> {
        let Some(document) = self.state.read_context_revision(revision_id).await? else {
            return Ok(None);
        };
        Hunch::from_document(&document).transpose()
    }

    async fn read_document(&self, document_id: &str) -> Result<Option<Hunch>> {
        let Some(document) = self.state.read_context_document(document_id).await? else {
            return Ok(None);
        };
        Hunch::from_document(&document).transpose()
    }
}

impl Hunch {
    fn from_document(document: &LocalContextDocument) -> Option<Result<Self>> {
        let facets = document.facets.as_ref()?.as_object()?;
        if facet_text(facets, "kind") != Some(HUNCH_KIND) {
            return None;
        }
        Some((|| {
            let question = required_facet(facets, "question")?;
            let why_alive = required_facet(facets, "whyAlive")?;
            let what_would_change_it = required_facet(facets, "whatWouldChangeIt")?;
            let origin = HunchOrigin::parse(required_facet(facets, "origin")?.as_str())
                .context("unknown Hunch origin")?;
            let state = HunchState::parse(required_facet(facets, "hunchState")?.as_str())
                .context("unknown Hunch state")?;
            Ok(Self {
                page_id: document.document_id.clone(),
                revision_id: document.revision_id.clone(),
                question,
                origin,
                why_alive,
                what_would_change_it,
                state,
                resolution: facet_text(facets, "resolution").map(str::to_owned),
                source_revision_ids: document.source_revision_ids.clone(),
                last_explored_at: facet_text(facets, "lastExploredAt").map(str::to_owned),
                attention: facet_text(facets, "attentionState")
                    .and_then(HunchAttention::parse)
                    .unwrap_or(HunchAttention::Ready),
                last_surfaced_message_revision_id: facet_text(
                    facets,
                    "lastSurfacedMessageRevisionId",
                )
                .map(str::to_owned),
                last_feedback_revision_id: facet_text(facets, "lastFeedbackRevisionId")
                    .map(str::to_owned),
                eligible_after: facet_text(facets, "eligibleAfter").map(str::to_owned),
                feedback_assessment: facet_text(facets, "feedbackAssessment").map(str::to_owned),
                updated_at: document.updated_at.clone(),
            })
        })())
    }
}

struct HunchFacetValues<'a> {
    question: &'a str,
    origin: HunchOrigin,
    why_alive: &'a str,
    what_would_change_it: &'a str,
    state: HunchState,
    resolution: Option<&'a str>,
    last_explored_at: Option<&'a str>,
    attention: HunchAttention,
    last_surfaced_message_revision_id: Option<&'a str>,
    last_feedback_revision_id: Option<&'a str>,
    eligible_after: Option<&'a str>,
    feedback_assessment: Option<&'a str>,
}

fn hunch_facets(values: HunchFacetValues<'_>) -> Value {
    let mut facets = Map::new();
    facets.insert("kind".to_owned(), json!(HUNCH_KIND));
    facets.insert("origin".to_owned(), json!(values.origin.as_str()));
    facets.insert("hunchState".to_owned(), json!(values.state.as_str()));
    facets.insert("question".to_owned(), json!(values.question.trim()));
    facets.insert("whyAlive".to_owned(), json!(values.why_alive.trim()));
    facets.insert(
        "whatWouldChangeIt".to_owned(),
        json!(values.what_would_change_it.trim()),
    );
    facets.insert(
        "attentionState".to_owned(),
        json!(values.attention.as_str()),
    );
    if let Some(value) = values.resolution {
        facets.insert("resolution".to_owned(), json!(value.trim()));
    }
    if let Some(value) = values.last_explored_at {
        facets.insert("lastExploredAt".to_owned(), json!(value));
    }
    if let Some(value) = values.last_surfaced_message_revision_id {
        facets.insert("lastSurfacedMessageRevisionId".to_owned(), json!(value));
    }
    if let Some(value) = values.last_feedback_revision_id {
        facets.insert("lastFeedbackRevisionId".to_owned(), json!(value));
    }
    if let Some(value) = values.eligible_after {
        facets.insert("eligibleAfter".to_owned(), json!(value));
    }
    if let Some(value) = values.feedback_assessment {
        facets.insert("feedbackAssessment".to_owned(), json!(value.trim()));
    }
    Value::Object(facets)
}

fn hunch_markdown(
    question: &str,
    why_alive: &str,
    what_would_change_it: &str,
    state: HunchState,
    resolution: Option<&str>,
) -> String {
    let mut content = format!(
        "# Hunch\n\n{}\n\n## Why it remains open\n\n{}\n\n## What would change it\n\n{}\n\nState: `{}`",
        question.trim(),
        why_alive.trim(),
        what_would_change_it.trim(),
        state.as_str()
    );
    if let Some(resolution) = resolution {
        content.push_str("\n\n## Resolution\n\n");
        content.push_str(resolution.trim());
    }
    content
}

fn prompt_hunch(hunch: &Hunch) -> String {
    format!(
        "Hunch `{}` revision `{}` [{}; origin={}]\n\
         Question: {}\n\
         Why alive: {}\n\
         What would change it: {}\n\
         Last explored: {}\n\
         Attention: {}; eligible after: {}\n\
         Last surfaced message: {}; last feedback: {}\n\
         Feedback assessment: {}",
        hunch.page_id,
        hunch.revision_id,
        hunch.state.as_str(),
        hunch.origin.as_str(),
        truncate(&hunch.question, MAX_PROMPT_FIELD_CHARS),
        truncate(&hunch.why_alive, MAX_PROMPT_FIELD_CHARS),
        truncate(&hunch.what_would_change_it, MAX_PROMPT_FIELD_CHARS),
        hunch.last_explored_at.as_deref().unwrap_or("never"),
        hunch.attention.as_str(),
        hunch.eligible_after.as_deref().unwrap_or("now"),
        hunch
            .last_surfaced_message_revision_id
            .as_deref()
            .unwrap_or("none"),
        hunch.last_feedback_revision_id.as_deref().unwrap_or("none"),
        hunch.feedback_assessment.as_deref().unwrap_or("none")
    )
}

fn required_facet(facets: &Map<String, Value>, key: &str) -> Result<String> {
    facet_text(facets, key)
        .map(str::to_owned)
        .with_context(|| format!("Hunch facet {key} is missing"))
}

fn facet_text<'a>(facets: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    facets
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn validate_field(label: &str, value: &str) -> Result<()> {
    let count = value.trim().chars().count();
    if count == 0 || count > MAX_HUNCH_FIELD_CHARS {
        anyhow::bail!("{label} must contain 1-{MAX_HUNCH_FIELD_CHARS} characters");
    }
    Ok(())
}

fn normalize_sources(source_revision_ids: Vec<String>) -> Vec<String> {
    source_revision_ids
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn feedback_cooldown_at() -> String {
    (Utc::now() + Duration::hours(FEEDBACK_COOLDOWN_HOURS))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_sqlite::SqlitePcpStore;

    use super::{CuriosityStore, HunchAttention, HunchOrigin, HunchPatch, HunchState, NewHunch};
    use crate::{
        continuity::{ContinuityHost, MessageLinks},
        memory::MemoryRole,
    };

    #[tokio::test]
    async fn hunches_revise_in_place_and_remain_separate_from_user_profile() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-curiosity-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP"),
        );
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
        let curiosity = CuriosityStore::new(Arc::clone(&continuity));
        let source = continuity
            .ingest_message(
                MemoryRole::User,
                "Could conversation itself wake a stronger exploration?",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store source");
        let created = curiosity
            .open(NewHunch {
                question: "Can conversation-created questions improve exploration diversity?"
                    .to_owned(),
                origin: HunchOrigin::Symbiont,
                why_alive: "The timer repeatedly follows the same nearby topic.".to_owned(),
                what_would_change_it: "Several event-driven runs either diversify or repeat."
                    .to_owned(),
                source_revision_ids: vec![source.page.revision_id.clone()],
            })
            .await
            .expect("open Hunch");
        let revised = curiosity
            .revise(
                &created.page_id,
                &created.revision_id,
                HunchPatch {
                    state: Some(HunchState::Watching),
                    why_alive: Some(
                        "A conversation event is now available as a separate wake source."
                            .to_owned(),
                    ),
                    source_revision_ids: vec![source.page.revision_id],
                    ..HunchPatch::default()
                },
                true,
            )
            .await
            .expect("revise Hunch");

        assert_eq!(created.page_id, revised.page_id);
        assert_ne!(created.revision_id, revised.revision_id);
        let snapshot = curiosity.snapshot().await.expect("read curiosity");
        assert_eq!(snapshot.active_count, 1);
        assert_eq!(snapshot.hunches[0].state, HunchState::Watching);
        assert!(snapshot.hunches[0].last_explored_at.is_some());
        let prompt = curiosity.prompt().await.expect("render prompt");
        assert!(prompt.contains("not the user's profile"));
        assert!(prompt.contains(&created.page_id));
        let exploration_prompt = curiosity
            .exploration_prompt()
            .await
            .expect("render exploration prompt");
        assert!(exploration_prompt.contains("ready-investigation-candidates"));
        assert!(exploration_prompt.contains(&created.page_id));

        drop(curiosity);
        drop(continuity);
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn surfaced_hunch_feedback_moves_through_pending_and_cooldown() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-curiosity-feedback-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP"),
        );
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
        let curiosity = CuriosityStore::new(Arc::clone(&continuity));
        let source = continuity
            .ingest_message(
                MemoryRole::User,
                "Keep an eye on whether this runtime can recover cleanly.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store source");
        let created = curiosity
            .open(NewHunch {
                question: "Can the runtime recover cleanly after interruption?".to_owned(),
                origin: HunchOrigin::User,
                why_alive: "Recovery has not been exercised end to end.".to_owned(),
                what_would_change_it: "A real interrupted run resumes without duplicate work."
                    .to_owned(),
                source_revision_ids: vec![source.page.revision_id],
            })
            .await
            .expect("open Hunch");
        let surfaced = continuity
            .ingest_message(
                MemoryRole::Assistant,
                "The recovery boundary still looks unresolved.",
                Vec::new(),
                None,
                MessageLinks {
                    responds_to: None,
                    continues_from: None,
                    input_revision_ids: Vec::new(),
                    surfaced_hunch_revision_ids: vec![created.revision_id.clone()],
                    quotes: Vec::new(),
                    topic: None,
                },
            )
            .await
            .expect("store surfaced message");
        let awaiting = curiosity
            .mark_surfaced(
                &created.page_id,
                &created.revision_id,
                &surfaced.page.revision_id,
            )
            .await
            .expect("mark surfaced")
            .expect("active Hunch");
        assert_eq!(
            continuity
                .surfaced_hunch_revisions(&surfaced.page.revision_id)
                .await
                .expect("read surfaced relation"),
            vec![created.revision_id.clone()]
        );

        let feedback = continuity
            .ingest_message(
                MemoryRole::User,
                "Yes, this is still unresolved, but wait until the resume path changes.",
                Vec::new(),
                None,
                MessageLinks {
                    responds_to: Some(surfaced.page.revision_id),
                    ..MessageLinks::default()
                },
            )
            .await
            .expect("store feedback");
        let pending = curiosity
            .mark_feedback_pending(&created.revision_id, &feedback.page.revision_id)
            .await
            .expect("mark feedback")
            .expect("active Hunch");
        let snapshot = curiosity.snapshot().await.expect("read pending Hunch");
        assert_eq!(
            snapshot.hunches[0].attention,
            HunchAttention::FeedbackPending
        );
        assert_eq!(
            snapshot.hunches[0].last_feedback_revision_id.as_deref(),
            Some(feedback.page.revision_id.as_str())
        );

        curiosity
            .acknowledge_feedback(
                &pending.page_id,
                &pending.revision_id,
                &feedback.page.revision_id,
                "The reply confirms the question but asks to wait for a concrete runtime change.",
            )
            .await
            .expect("acknowledge feedback");
        let snapshot = curiosity.snapshot().await.expect("read cooldown Hunch");
        assert_eq!(snapshot.hunches[0].attention, HunchAttention::Cooldown);
        assert!(snapshot.hunches[0].eligible_after.is_some());
        assert_eq!(awaiting.page_id, created.page_id);
        let exploration_prompt = curiosity
            .exploration_prompt()
            .await
            .expect("render constrained exploration prompt");
        assert!(exploration_prompt.contains("No Hunch is currently ready"));
        assert!(exploration_prompt.contains("excluded-investigations"));
        assert!(exploration_prompt.contains("cooldown"));

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn resolved_hunches_leave_the_active_map_without_being_deleted() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-curiosity-retire-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP"),
        );
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
        let curiosity = CuriosityStore::new(continuity);
        let created = curiosity
            .open(NewHunch {
                question: "Will this remain open?".to_owned(),
                origin: HunchOrigin::External,
                why_alive: "There is one unresolved claim.".to_owned(),
                what_would_change_it: "A primary source settles the claim.".to_owned(),
                source_revision_ids: Vec::new(),
            })
            .await
            .expect("open Hunch");
        curiosity
            .retire(
                &created.page_id,
                &created.revision_id,
                HunchState::Resolved,
                Some("The primary source settled it.".to_owned()),
                Vec::new(),
                true,
            )
            .await
            .expect("resolve Hunch");

        let snapshot = curiosity.snapshot().await.expect("read curiosity");
        assert_eq!(snapshot.active_count, 0);
        assert_eq!(snapshot.hunches.len(), 1);
        assert_eq!(snapshot.hunches[0].state, HunchState::Resolved);

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
