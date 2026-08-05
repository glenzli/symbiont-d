use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{
    Projection, SearchFilters, SearchHit, SearchMode, SearchPagesRequest, SearchTermMatch,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    asset::AssetStore,
    continuity::{ContinuityHost, ImageAssetPage},
    curiosity::{CuriosityStore, HunchState},
    memory::{MemoryEntry, MemoryRole},
    profile::ProfileStore,
    reflection::{ConversationEpisode, EpisodeState, ReflectionStore},
    symbiont_context::SymbiontContextStore,
};

const MAX_QUERY_CHARS: usize = 500;
const MAX_PURPOSE_CHARS: usize = 500;
const MAX_ORIENTATION_CHARS: usize = 4_000;
const MAX_CONTEXT_DOCUMENT_CHARS: usize = 3_000;
const MAX_SIGNAL_CHARS: usize = 700;
const MAX_SIGNALS: usize = 6;
const MAX_RECALLS: u32 = 6;
const MAX_BRIDGE_IMAGES: usize = 4;
const MAX_TOPIC_CANDIDATES: usize = 80;
const DEFAULT_TOKEN_BUDGET: usize = 6_000;
const MIN_TOKEN_BUDGET: usize = 1_000;
const MAX_TOKEN_BUDGET: usize = 24_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSignal {
    pub text: String,
    pub state: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRecall {
    pub revision_id: String,
    pub namespace: String,
    pub snippet: String,
    pub matched_projection: String,
    pub observed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeImage {
    pub revision_id: String,
    pub asset_id: String,
    pub local_path: Option<String>,
    pub url: String,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: usize,
    pub width: u32,
    pub height: u32,
    pub observed_at: String,
    pub attached_to_revision_id: Option<String>,
    pub source_type: Option<String>,
    pub context: Option<String>,
    pub matched_by: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeContextPacket {
    pub generated_at: String,
    pub query: Option<String>,
    pub orientation: String,
    pub current_map: Option<String>,
    pub open_loops: Option<String>,
    pub active_hunches: Vec<BridgeSignal>,
    pub working_hypotheses: Vec<BridgeSignal>,
    pub recalled_pages: Vec<BridgeRecall>,
    pub images: Vec<BridgeImage>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeRecallDepth {
    Brief,
    #[default]
    Normal,
    Deep,
}

impl BridgeRecallDepth {
    fn topic_limit(self) -> usize {
        match self {
            Self::Brief => 1,
            Self::Normal => 3,
            Self::Deep => 5,
        }
    }

    fn page_limit(self) -> u32 {
        match self {
            Self::Brief => 6,
            Self::Normal => 12,
            Self::Deep => 24,
        }
    }

    fn evidence_limit(self) -> usize {
        match self {
            Self::Brief => 6,
            Self::Normal => 18,
            Self::Deep => 48,
        }
    }

    fn evidence_chars(self) -> usize {
        match self {
            Self::Brief => 700,
            Self::Normal => 1_400,
            Self::Deep => 2_400,
        }
    }

    fn related_context_limit(self) -> usize {
        match self {
            Self::Brief => 2,
            Self::Normal => 4,
            Self::Deep => 8,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRecallRequest {
    pub query: String,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub depth: BridgeRecallDepth,
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeUnderstanding {
    pub topic_id: String,
    pub title: String,
    pub summary: String,
    pub state: String,
    pub started_at: String,
    pub last_activity_at: String,
    pub updated_at: String,
    pub source_revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRelatedContext {
    pub source: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEvidence {
    pub revision_id: String,
    pub role: MemoryRole,
    pub at: String,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub topic_ids: Vec<String>,
    pub matched_query: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSupportingPage {
    pub revision_id: String,
    pub namespace: String,
    pub kind: String,
    pub snippet: String,
    pub matched_by: String,
    pub matched_projection: String,
    pub observed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRecallBundle {
    pub bundle_id: String,
    pub generated_at: String,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub depth: BridgeRecallDepth,
    pub token_budget: usize,
    pub current_understanding: Vec<BridgeUnderstanding>,
    pub related_context: Vec<BridgeRelatedContext>,
    pub evidence: Vec<BridgeEvidence>,
    pub supporting_pages: Vec<BridgeSupportingPage>,
    pub images: Vec<BridgeImage>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeExpandRequest {
    pub topic_id: String,
    #[serde(default = "default_expand_limit")]
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRecallExpansion {
    pub generated_at: String,
    pub topic: BridgeUnderstanding,
    pub evidence: Vec<BridgeEvidence>,
    pub images: Vec<BridgeImage>,
    pub truncated: bool,
}

pub(super) struct RecallService {
    continuity: Arc<ContinuityHost>,
    profile: Arc<ProfileStore>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    assets: Arc<AssetStore>,
}

impl RecallService {
    pub(super) fn new(
        continuity: Arc<ContinuityHost>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        assets: Arc<AssetStore>,
    ) -> Self {
        Self {
            continuity,
            profile,
            context,
            curiosity,
            reflection,
            assets,
        }
    }

    pub(super) async fn context_packet(&self, query: Option<&str>) -> Result<BridgeContextPacket> {
        let query = query
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| truncate(value, MAX_QUERY_CHARS));
        let profile = self.profile.snapshot().await;
        let context = self.context.snapshot().await?;
        let curiosity = self.curiosity.snapshot().await?;
        let hypotheses = self.reflection.hypotheses(MAX_SIGNALS).await?;
        let recalled_pages: Vec<BridgeRecall> = if let Some(query) = query.as_deref() {
            self.continuity
                .search(SearchPagesRequest {
                    query: query.to_owned(),
                    scopes: Vec::new(),
                    mode: SearchMode::Text,
                    term_match: SearchTermMatch::Any,
                    projections: vec![Projection::Summary, Projection::Payload],
                    filters: SearchFilters::default(),
                    limit: MAX_RECALLS,
                    cursor: None,
                })
                .await?
                .hits
                .into_iter()
                .map(|hit| BridgeRecall {
                    revision_id: hit.revision_id,
                    namespace: hit.namespace,
                    snippet: truncate(&hit.snippet, MAX_SIGNAL_CHARS),
                    matched_projection: hit.matched_projection,
                    observed_at: hit.observed_at,
                })
                .collect()
        } else {
            Vec::new()
        };
        let recalled_context = recalled_pages
            .iter()
            .map(|page| (page.revision_id.clone(), page.snippet.clone()))
            .collect::<HashMap<_, _>>();
        let recalled_revision_ids = recalled_pages
            .iter()
            .map(|page| page.revision_id.clone())
            .collect::<Vec<_>>();
        let related_image_revision_ids = self
            .continuity
            .attached_image_revision_ids(&recalled_revision_ids)
            .await?;
        let related_image_set = related_image_revision_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let mut image_pages = self
            .continuity
            .read_image_assets(&related_image_revision_ids)
            .await?;
        image_pages.extend(
            self.continuity
                .recent_image_assets(MAX_BRIDGE_IMAGES)
                .await?,
        );
        let mut seen_images = HashSet::new();
        image_pages.retain(|image| seen_images.insert(image.revision_id.clone()));
        image_pages.truncate(MAX_BRIDGE_IMAGES);
        let images = self
            .project_images(image_pages, &related_image_set, &recalled_context)
            .await;
        Ok(BridgeContextPacket {
            generated_at: now(),
            query,
            orientation: truncate(&profile.orientation, MAX_ORIENTATION_CHARS),
            current_map: context
                .current_map
                .map(|document| truncate(&document.content, MAX_CONTEXT_DOCUMENT_CHARS)),
            open_loops: context
                .open_loops
                .map(|document| truncate(&document.content, MAX_CONTEXT_DOCUMENT_CHARS)),
            active_hunches: curiosity
                .hunches
                .into_iter()
                .filter(|hunch| {
                    matches!(hunch.state, HunchState::Germinating | HunchState::Watching)
                })
                .take(MAX_SIGNALS)
                .map(|hunch| BridgeSignal {
                    text: truncate(&hunch.question, MAX_SIGNAL_CHARS),
                    state: hunch.state.as_str().to_owned(),
                    updated_at: hunch.updated_at,
                })
                .collect(),
            working_hypotheses: hypotheses
                .into_iter()
                .filter(|hypothesis| hypothesis.status.is_active())
                .take(MAX_SIGNALS)
                .map(|hypothesis| BridgeSignal {
                    text: truncate(&hypothesis.statement, MAX_SIGNAL_CHARS),
                    state: hypothesis.status.as_str().to_owned(),
                    updated_at: hypothesis.updated_at,
                })
                .collect(),
            recalled_pages,
            images,
        })
    }

    pub(super) async fn recall(&self, request: BridgeRecallRequest) -> Result<BridgeRecallBundle> {
        let query = required_text(request.query, "query", MAX_QUERY_CHARS)?;
        let purpose = optional_text(request.purpose, MAX_PURPOSE_CHARS);
        let token_budget = request
            .token_budget
            .clamp(MIN_TOKEN_BUDGET, MAX_TOKEN_BUDGET);
        let generated_at = now();

        let episodes = self.reflection.episodes(MAX_TOPIC_CANDIDATES).await?;
        let selected_episodes = select_episodes(&query, episodes, request.depth.topic_limit());
        let current_understanding = selected_episodes
            .iter()
            .cloned()
            .map(BridgeUnderstanding::from)
            .collect::<Vec<_>>();
        let related_context = self
            .related_context(&query, request.depth.related_context_limit())
            .await?;

        let page_hits = self
            .search_pages(&query, request.depth.page_limit())
            .await?;
        let supporting_pages = page_hits
            .iter()
            .map(|hit| BridgeSupportingPage {
                revision_id: hit.revision_id.clone(),
                namespace: hit.namespace.clone(),
                kind: hit.kind.clone(),
                snippet: truncate(&hit.snippet, MAX_SIGNAL_CHARS),
                matched_by: hit.matched_by.clone(),
                matched_projection: hit.matched_projection.clone(),
                observed_at: hit.observed_at.clone(),
            })
            .collect::<Vec<_>>();

        let (evidence, evidence_truncated) = self
            .collect_evidence(&selected_episodes, &page_hits, request.depth, token_budget)
            .await?;
        let mut recalled_context = supporting_pages
            .iter()
            .map(|page| (page.revision_id.clone(), page.snippet.clone()))
            .collect::<HashMap<_, _>>();
        recalled_context.extend(
            evidence
                .iter()
                .map(|item| (item.revision_id.clone(), item.content.clone())),
        );
        let related_revision_ids = supporting_pages
            .iter()
            .map(|page| page.revision_id.clone())
            .chain(evidence.iter().map(|item| item.revision_id.clone()))
            .collect::<Vec<_>>();
        let images = self
            .images_for_revisions(&related_revision_ids, &recalled_context)
            .await?;

        Ok(BridgeRecallBundle {
            bundle_id: bundle_id(&query, purpose.as_deref(), &generated_at),
            generated_at,
            query,
            purpose,
            depth: request.depth,
            token_budget,
            current_understanding,
            related_context,
            evidence,
            supporting_pages,
            images,
            truncated: evidence_truncated,
        })
    }

    pub(super) async fn expand(
        &self,
        request: BridgeExpandRequest,
    ) -> Result<BridgeRecallExpansion> {
        let topic_id = required_text(request.topic_id, "topicId", 128)?;
        if !topic_id.starts_with("ep_") {
            anyhow::bail!("invalid conversation Topic ID");
        }
        let topic = self
            .reflection
            .episode(&topic_id)
            .await?
            .with_context(|| format!("conversation Topic {topic_id} was not found"))?;
        let limit = request.limit.clamp(1, 200);
        let revision_ids = self
            .reflection
            .episode_revision_ids(&topic.id, limit + 1)
            .await?;
        let truncated = revision_ids.len() > limit;
        let revision_ids = revision_ids.into_iter().take(limit).collect::<Vec<_>>();
        let messages = self
            .continuity
            .messages_by_revision_ids(&revision_ids)
            .await?;
        let evidence = messages
            .into_iter()
            .filter_map(|message| {
                evidence_from_message(message, std::slice::from_ref(&topic.id), false, usize::MAX)
            })
            .collect::<Vec<_>>();
        let context = evidence
            .iter()
            .map(|item| (item.revision_id.clone(), item.content.clone()))
            .collect::<HashMap<_, _>>();
        let images = self.images_for_revisions(&revision_ids, &context).await?;
        Ok(BridgeRecallExpansion {
            generated_at: now(),
            topic: topic.into(),
            evidence,
            images,
            truncated,
        })
    }

    async fn search_pages(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let mut hits = self
            .search_pages_with(query, limit, SearchTermMatch::All)
            .await?;
        if hits.len() < (limit as usize / 2).max(2) {
            let fallback = self
                .search_pages_with(query, limit, SearchTermMatch::Any)
                .await?;
            let mut seen = hits
                .iter()
                .map(|hit| hit.revision_id.clone())
                .collect::<HashSet<_>>();
            hits.extend(
                fallback
                    .into_iter()
                    .filter(|hit| seen.insert(hit.revision_id.clone())),
            );
        }
        hits.truncate(limit as usize);
        Ok(hits)
    }

    async fn search_pages_with(
        &self,
        query: &str,
        limit: u32,
        term_match: SearchTermMatch,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .continuity
            .search(SearchPagesRequest {
                query: query.to_owned(),
                scopes: Vec::new(),
                mode: SearchMode::Text,
                term_match,
                projections: vec![Projection::Summary, Projection::Payload],
                filters: SearchFilters::default(),
                limit,
                cursor: None,
            })
            .await?
            .hits)
    }

    async fn related_context(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<BridgeRelatedContext>> {
        let profile = self.profile.snapshot().await;
        let context = self.context.snapshot().await?;
        let curiosity = self.curiosity.snapshot().await?;
        let hypotheses = self.reflection.hypotheses(MAX_SIGNALS * 2).await?;
        let mut candidates = Vec::new();
        push_related(
            &mut candidates,
            query,
            "orientation",
            profile.orientation,
            None,
            None,
        );
        if let Some(document) = context.current_map {
            push_related(
                &mut candidates,
                query,
                "current_map",
                document.content,
                None,
                Some(document.updated_at),
            );
        }
        if let Some(document) = context.open_loops {
            push_related(
                &mut candidates,
                query,
                "open_loops",
                document.content,
                None,
                Some(document.updated_at),
            );
        }
        for hunch in curiosity
            .hunches
            .into_iter()
            .filter(|hunch| matches!(hunch.state, HunchState::Germinating | HunchState::Watching))
        {
            push_related(
                &mut candidates,
                query,
                "hunch",
                hunch.question,
                Some(hunch.state.as_str().to_owned()),
                Some(hunch.updated_at),
            );
        }
        for hypothesis in hypotheses
            .into_iter()
            .filter(|hypothesis| hypothesis.status.is_active())
        {
            push_related(
                &mut candidates,
                query,
                "working_hypothesis",
                hypothesis.statement,
                Some(hypothesis.status.as_str().to_owned()),
                Some(hypothesis.updated_at),
            );
        }
        candidates
            .sort_by_key(|(score, context)| (Reverse(*score), Reverse(context.updated_at.clone())));
        Ok(candidates
            .into_iter()
            .take(limit)
            .map(|(_, context)| context)
            .collect())
    }

    async fn collect_evidence(
        &self,
        episodes: &[ConversationEpisode],
        page_hits: &[SearchHit],
        depth: BridgeRecallDepth,
        token_budget: usize,
    ) -> Result<(Vec<BridgeEvidence>, bool)> {
        let mut topic_ids_by_revision = HashMap::<String, Vec<String>>::new();
        let mut candidate_ids = Vec::new();
        for episode in episodes {
            let revision_ids = self
                .reflection
                .episode_revision_ids(&episode.id, depth.evidence_limit())
                .await?;
            for revision_id in revision_ids {
                topic_ids_by_revision
                    .entry(revision_id.clone())
                    .or_default()
                    .push(episode.id.clone());
                candidate_ids.push(revision_id);
            }
        }

        let direct_conversation_ids = page_hits
            .iter()
            .filter(|hit| {
                hit.namespace.starts_with("conversation:") && hit.kind == "conversation_event"
            })
            .map(|hit| hit.revision_id.clone())
            .collect::<Vec<_>>();
        let matched_turn_ids = if direct_conversation_ids.is_empty() {
            HashSet::new()
        } else {
            match self
                .reflection
                .conversation_turn_revision_ids(&direct_conversation_ids)
                .await
            {
                Ok(revision_ids) => revision_ids.into_iter().collect(),
                Err(error) => {
                    warn!(%error, "could not expand recalled conversation turns; using direct matches");
                    direct_conversation_ids.iter().cloned().collect()
                }
            }
        };
        candidate_ids.extend(matched_turn_ids.iter().cloned());
        candidate_ids.sort();
        candidate_ids.dedup();
        let messages = self
            .continuity
            .messages_by_revision_ids(&candidate_ids)
            .await?;

        let max_messages = depth.evidence_limit();
        let total_candidates = messages.len();
        let selected = select_messages(messages, &matched_turn_ids, max_messages);
        let evidence_char_budget = token_budget
            .saturating_mul(4)
            .saturating_mul(3)
            .saturating_div(5);
        let mut remaining_chars = evidence_char_budget;
        let mut evidence = Vec::new();
        for message in selected {
            if remaining_chars == 0 {
                break;
            }
            let Some(revision_id) = message.revision_id.clone() else {
                continue;
            };
            let max_chars = depth.evidence_chars().min(remaining_chars);
            if let Some(item) = evidence_from_message(
                message,
                topic_ids_by_revision
                    .get(&revision_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                matched_turn_ids.contains(&revision_id),
                max_chars,
            ) {
                remaining_chars = remaining_chars.saturating_sub(item.content.chars().count());
                evidence.push(item);
            }
        }
        let truncated = total_candidates > evidence.len() || remaining_chars == 0;
        Ok((evidence, truncated))
    }

    async fn images_for_revisions(
        &self,
        revision_ids: &[String],
        recalled_context: &HashMap<String, String>,
    ) -> Result<Vec<BridgeImage>> {
        let mut unique_revision_ids = revision_ids.to_vec();
        unique_revision_ids.sort();
        unique_revision_ids.dedup();
        let mut image_revision_ids = Vec::new();
        for chunk in unique_revision_ids.chunks(20) {
            image_revision_ids.extend(self.continuity.attached_image_revision_ids(chunk).await?);
        }
        image_revision_ids.sort();
        image_revision_ids.dedup();
        let image_set = image_revision_ids.iter().cloned().collect::<HashSet<_>>();
        let mut images = self
            .continuity
            .read_image_assets(&image_revision_ids)
            .await?;
        images.truncate(MAX_BRIDGE_IMAGES);
        Ok(self
            .project_images(images, &image_set, recalled_context)
            .await)
    }

    async fn project_images(
        &self,
        images: Vec<ImageAssetPage>,
        related_images: &HashSet<String>,
        recalled_context: &HashMap<String, String>,
    ) -> Vec<BridgeImage> {
        let mut projected = Vec::with_capacity(images.len());
        for image in images {
            let local_path = self
                .assets
                .local_path(&image.attachment.asset_id)
                .await
                .ok()
                .map(|path| path.to_string_lossy().into_owned());
            let context = image
                .attached_to_revision_id
                .as_ref()
                .and_then(|revision_id| recalled_context.get(revision_id))
                .cloned()
                .or_else(|| image.revised_prompt.clone())
                .map(|value| truncate(&value, MAX_SIGNAL_CHARS));
            projected.push(BridgeImage {
                revision_id: image.revision_id.clone(),
                asset_id: image.attachment.asset_id,
                local_path,
                url: image.attachment.url,
                filename: image.attachment.filename,
                mime_type: image.attachment.mime_type,
                byte_size: image.attachment.byte_size,
                width: image.attachment.width,
                height: image.attachment.height,
                observed_at: image.observed_at,
                attached_to_revision_id: image.attached_to_revision_id,
                source_type: image.source_type,
                context,
                matched_by: if related_images.contains(&image.revision_id) {
                    "query_relation".to_owned()
                } else {
                    "recent".to_owned()
                },
            });
        }
        projected
    }
}

impl From<ConversationEpisode> for BridgeUnderstanding {
    fn from(topic: ConversationEpisode) -> Self {
        Self {
            topic_id: topic.id,
            title: topic.title,
            summary: topic.summary,
            state: topic.state.as_str().to_owned(),
            started_at: topic.started_at,
            last_activity_at: topic.last_activity_at,
            updated_at: topic.updated_at,
            source_revision_ids: topic.source_revision_ids,
        }
    }
}

fn default_token_budget() -> usize {
    DEFAULT_TOKEN_BUDGET
}

fn default_expand_limit() -> usize {
    80
}

fn required_text(value: String, field: &str, max_chars: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    Ok(truncate(value, max_chars))
}

fn optional_text(value: Option<String>, max_chars: usize) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|value| truncate(&value, max_chars))
}

fn select_episodes(
    query: &str,
    episodes: Vec<ConversationEpisode>,
    limit: usize,
) -> Vec<ConversationEpisode> {
    let mut scored = episodes
        .into_iter()
        .filter_map(|episode| {
            let score = episode_score(query, &episode);
            (score > 0).then_some((score, episode))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(score, episode)| {
        (
            Reverse(*score),
            Reverse(episode_state_weight(episode.state)),
            Reverse(episode.last_activity_at.clone()),
        )
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, episode)| episode)
        .collect()
}

fn episode_score(query: &str, episode: &ConversationEpisode) -> usize {
    relevance_score(query, &episode.title) * 3 + relevance_score(query, &episode.summary)
}

fn episode_state_weight(state: EpisodeState) -> usize {
    match state {
        EpisodeState::Active => 4,
        EpisodeState::Forming => 3,
        EpisodeState::Dormant => 2,
        EpisodeState::Closed => 1,
    }
}

fn relevance_score(query: &str, candidate: &str) -> usize {
    let query = query.to_lowercase();
    let candidate = candidate.to_lowercase();
    if query.is_empty() || candidate.is_empty() {
        return 0;
    }
    let mut score = usize::from(candidate.contains(&query)) * 24;
    for term in query_terms(&query) {
        if candidate.contains(&term) {
            score += if term.chars().count() >= 6 { 6 } else { 3 };
        }
    }
    score
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms = query
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    ',' | '，'
                        | '.'
                        | '。'
                        | ':'
                        | '：'
                        | ';'
                        | '；'
                        | '/'
                        | '\\'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                )
        })
        .map(str::trim)
        .filter(|term| term.chars().count() >= 2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms
}

fn push_related(
    candidates: &mut Vec<(usize, BridgeRelatedContext)>,
    query: &str,
    source: &str,
    text: String,
    state: Option<String>,
    updated_at: Option<String>,
) {
    let score = relevance_score(query, &text);
    if score == 0 {
        return;
    }
    candidates.push((
        score,
        BridgeRelatedContext {
            source: source.to_owned(),
            text: truncate(&text, MAX_CONTEXT_DOCUMENT_CHARS),
            state,
            updated_at,
        },
    ));
}

fn select_messages(
    messages: Vec<MemoryEntry>,
    matched_turn_ids: &HashSet<String>,
    limit: usize,
) -> Vec<MemoryEntry> {
    if messages.len() <= limit {
        return messages;
    }
    let len = messages.len();
    let mut scored = messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let matched = message
                .revision_id
                .as_ref()
                .is_some_and(|revision_id| matched_turn_ids.contains(revision_id));
            let role_weight = match message.role {
                MemoryRole::User => 30,
                MemoryRole::Assistant => 20,
                MemoryRole::Memory => 10,
            };
            let boundary_weight = usize::from(index < 2 || index + 4 >= len) * 12;
            (
                usize::from(matched) * 100 + role_weight + boundary_weight,
                index,
                message,
            )
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(score, index, _)| (Reverse(*score), *index));
    scored.truncate(limit);
    scored.sort_by_key(|(_, index, _)| *index);
    scored.into_iter().map(|(_, _, message)| message).collect()
}

fn evidence_from_message(
    message: MemoryEntry,
    topic_ids: &[String],
    matched_query: bool,
    max_chars: usize,
) -> Option<BridgeEvidence> {
    let revision_id = message.revision_id?;
    Some(BridgeEvidence {
        revision_id,
        role: message.role,
        at: message.at,
        content: truncate(&message.content, max_chars),
        topic_ids: topic_ids.to_vec(),
        matched_query,
    })
}

fn bundle_id(query: &str, purpose: Option<&str>, generated_at: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(query.as_bytes());
    digest.update([0]);
    digest.update(purpose.unwrap_or_default().as_bytes());
    digest.update([0]);
    digest.update(generated_at.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    format!("rec_{}", &digest[..16])
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_sqlite::SqlitePcpStore;

    use super::{
        BridgeExpandRequest, BridgeRecallDepth, BridgeRecallRequest, RecallService, relevance_score,
    };
    use crate::{
        asset::AssetStore,
        continuity::{ContinuityHost, MessageLinks},
        curiosity::CuriosityStore,
        memory::MemoryRole,
        profile::ProfileStore,
        reflection::{EpisodeInput, EpisodeState, ReflectionStore},
        symbiont_context::SymbiontContextStore,
    };

    #[test]
    fn relevance_prefers_exact_and_topic_terms() {
        assert!(
            relevance_score("PCP summary 可变", "PCP summary Page 可以保持可变")
                > relevance_score("PCP summary 可变", "PCP runtime maintenance")
        );
        assert_eq!(relevance_score("PCP summary", "摄影工作流"), 0);
    }

    #[tokio::test]
    async fn recall_prefers_topic_understanding_and_keeps_both_sides_of_the_turn() {
        let fixture = Fixture::open().await;
        let user = fixture
            .continuity
            .ingest_message(
                MemoryRole::User,
                "Only original PCP Pages must remain immutable; summaries may evolve.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store user message");
        fixture
            .reflection
            .record_message(&user.entry, None, false, &[])
            .await
            .expect("reflect user message");
        let assistant = fixture
            .continuity
            .ingest_message(
                MemoryRole::Assistant,
                "Then a mutable summary head should retain provenance to immutable evidence.",
                Vec::new(),
                None,
                MessageLinks {
                    responds_to: Some(user.page.revision_id.clone()),
                    ..MessageLinks::default()
                },
            )
            .await
            .expect("store assistant message");
        fixture
            .reflection
            .record_message(&assistant.entry, Some(&user.page.revision_id), false, &[])
            .await
            .expect("reflect assistant message");
        let topic = fixture
            .reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Mutable PCP summary Pages".to_owned(),
                summary: "Original Pages remain immutable while summary heads can evolve with provenance."
                    .to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![user.page.revision_id.clone()],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("create topic");
        fixture
            .reflection
            .attach_episode_messages(&topic.id, std::slice::from_ref(&assistant.page.revision_id))
            .await
            .expect("attach assistant");

        let bundle = fixture
            .service()
            .recall(BridgeRecallRequest {
                query: "PCP summary mutable".to_owned(),
                purpose: Some("review the current implementation".to_owned()),
                depth: BridgeRecallDepth::Normal,
                token_budget: 6_000,
            })
            .await
            .expect("recall context");

        assert_eq!(bundle.current_understanding.len(), 1);
        assert_eq!(bundle.current_understanding[0].topic_id, topic.id);
        assert!(
            bundle.current_understanding[0]
                .summary
                .contains("provenance")
        );
        assert!(
            bundle
                .evidence
                .iter()
                .any(|item| item.role == MemoryRole::User)
        );
        assert!(
            bundle
                .evidence
                .iter()
                .any(|item| item.role == MemoryRole::Assistant)
        );
        assert!(bundle.related_context.is_empty());

        let expansion = fixture
            .service()
            .expand(BridgeExpandRequest {
                topic_id: topic.id,
                limit: 20,
            })
            .await
            .expect("expand topic");
        assert_eq!(expansion.evidence.len(), 2);
        assert!(!expansion.truncated);
    }

    #[tokio::test]
    async fn recall_inspects_images_in_bounded_revision_batches() {
        let fixture = Fixture::open().await;
        let mut revision_ids = Vec::new();
        for index in 0..22 {
            let message = fixture
                .continuity
                .ingest_message(
                    MemoryRole::User,
                    &format!("Recall batching evidence marker {index}"),
                    Vec::new(),
                    None,
                    MessageLinks::default(),
                )
                .await
                .expect("store evidence message");
            fixture
                .reflection
                .record_message(&message.entry, None, false, &[])
                .await
                .expect("reflect evidence message");
            revision_ids.push(message.page.revision_id);
        }
        let topic = fixture
            .reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Recall batching evidence".to_owned(),
                summary: "A large evidence set still performs attachment lookup in safe batches."
                    .to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: revision_ids.clone(),
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("create large topic");
        fixture
            .reflection
            .attach_episode_messages(&topic.id, &revision_ids)
            .await
            .expect("attach evidence messages");

        let bundle = fixture
            .service()
            .recall(BridgeRecallRequest {
                query: "Recall batching evidence".to_owned(),
                purpose: None,
                depth: BridgeRecallDepth::Deep,
                token_budget: 24_000,
            })
            .await
            .expect("recall more than twenty revisions");

        assert_eq!(bundle.current_understanding[0].topic_id, topic.id);
        assert!(bundle.evidence.len() > 20);
        assert!(bundle.images.is_empty());
    }

    struct Fixture {
        root: std::path::PathBuf,
        continuity: Arc<ContinuityHost>,
        reflection: Arc<ReflectionStore>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        assets: Arc<AssetStore>,
    }

    impl Fixture {
        async fn open() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("symbiont-recall-{nonce}"));
            let pcp = Arc::new(
                SqlitePcpStore::open(root.join("context.sqlite3"))
                    .await
                    .expect("open PCP store"),
            );
            let continuity = Arc::new(
                ContinuityHost::open_embedded_for_test(pcp)
                    .await
                    .expect("open continuity"),
            );
            let reflection = Arc::new(
                ReflectionStore::open(
                    root.join("reflection.sqlite3"),
                    root.join("reflection.toml"),
                )
                .await
                .expect("open reflection"),
            );
            let profile = Arc::new(
                ProfileStore::open(root.join("profile.toml"), root.join("orientation.md"))
                    .await
                    .expect("open profile"),
            );
            let context = Arc::new(SymbiontContextStore::new(Arc::clone(&continuity)));
            let curiosity = Arc::new(CuriosityStore::new(Arc::clone(&continuity)));
            let assets = Arc::new(
                AssetStore::open(root.join("assets"))
                    .await
                    .expect("open assets"),
            );
            Self {
                root,
                continuity,
                reflection,
                profile,
                context,
                curiosity,
                assets,
            }
        }

        fn service(&self) -> RecallService {
            RecallService::new(
                Arc::clone(&self.continuity),
                Arc::clone(&self.profile),
                Arc::clone(&self.context),
                Arc::clone(&self.curiosity),
                Arc::clone(&self.reflection),
                Arc::clone(&self.assets),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
