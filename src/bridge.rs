use std::{
    collections::{HashMap, HashSet},
    io::ErrorKind,
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{Projection, SearchFilters, SearchMode, SearchPagesRequest};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::{
    asset::AssetStore,
    codex::{CodexTaskDetail, CodexTaskSources, CodexTaskSummary},
    continuity::{ContinuityHost, ImageAssetPage},
    curiosity::{CuriosityStore, HunchState},
    profile::ProfileStore,
    reflection::{HypothesisStatus, ReflectionStore},
    symbiont_context::SymbiontContextStore,
};

const MAX_QUERY_CHARS: usize = 500;
const MAX_ORIENTATION_CHARS: usize = 4_000;
const MAX_CONTEXT_DOCUMENT_CHARS: usize = 3_000;
const MAX_SIGNAL_CHARS: usize = 700;
const MAX_SIGNALS: usize = 6;
const MAX_RECALLS: u32 = 6;
const MAX_BRIDGE_IMAGES: usize = 4;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeConfig {
    #[serde(default)]
    pub codex_task_access: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSnapshot {
    pub codex_task_access: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSettingsDraft {
    pub codex_task_access: bool,
}

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

pub struct CodexBridge {
    path: PathBuf,
    config: RwLock<BridgeConfig>,
    task_sources: Arc<CodexTaskSources>,
    continuity: Arc<ContinuityHost>,
    profile: Arc<ProfileStore>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    assets: Arc<AssetStore>,
}

impl CodexBridge {
    pub async fn open(
        path: PathBuf,
        task_sources: Arc<CodexTaskSources>,
        continuity: Arc<ContinuityHost>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        assets: Arc<AssetStore>,
    ) -> Result<Self> {
        let config = match fs::read_to_string(&path).await {
            Ok(content) => toml::from_str(&content)
                .with_context(|| format!("parse Codex bridge config {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => BridgeConfig::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read Codex bridge config {}", path.display()));
            }
        };
        let bridge = Self {
            path,
            config: RwLock::new(config),
            task_sources,
            continuity,
            profile,
            context,
            curiosity,
            reflection,
            assets,
        };
        bridge.persist().await?;
        Ok(bridge)
    }

    pub async fn snapshot(&self) -> BridgeSnapshot {
        let config = self.config.read().await.clone();
        BridgeSnapshot {
            codex_task_access: config.codex_task_access,
        }
    }

    pub async fn update_settings(&self, draft: BridgeSettingsDraft) -> Result<BridgeSnapshot> {
        let mut config = self.config.read().await.clone();
        config.codex_task_access = draft.codex_task_access;
        let content = toml::to_string_pretty(&config).context("encode Codex bridge config")?;
        persist(&self.path, &content).await?;
        *self.config.write().await = config.clone();
        Ok(self.snapshot().await)
    }

    pub async fn task_access_enabled(&self) -> bool {
        self.config.read().await.codex_task_access
    }

    pub async fn prompt(&self) -> String {
        "Host Codex bridge: Codex tasks are optional read-only external sources. Do not create, \
         resume, bind, or execute Codex tasks from Symbiont. The user may explicitly copy a \
         context packet into Codex when they want to act on this discussion."
            .to_owned()
    }

    pub async fn list_tasks(&self, refresh: bool) -> Result<Vec<CodexTaskSummary>> {
        self.task_sources.list(refresh).await
    }

    pub async fn read_task(&self, thread_id: &str) -> Result<CodexTaskDetail> {
        self.task_sources.read(thread_id).await
    }

    pub async fn context_packet(&self, query: Option<&str>) -> Result<BridgeContextPacket> {
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
                    term_match: pcp_core::SearchTermMatch::Any,
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
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
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
                .filter(|hypothesis| {
                    matches!(
                        hypothesis.status,
                        HypothesisStatus::Tentative | HypothesisStatus::Working
                    )
                })
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

    async fn persist(&self) -> Result<()> {
        let content = toml::to_string_pretty(&*self.config.read().await)
            .context("encode Codex bridge config")?;
        persist(&self.path, &content).await
    }
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

async fn persist(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create Codex bridge directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write Codex bridge config {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace Codex bridge config {}", path.display()))
}
