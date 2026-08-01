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
use tokio::{
    fs,
    sync::{Mutex, RwLock},
};

use crate::{
    asset::AssetStore,
    codex::{CodexClient, CodexTaskDetail, CodexTaskSummary},
    continuity::{ContinuityHost, ImageAssetPage},
    curiosity::{CuriosityStore, HunchState},
    profile::ProfileStore,
    reflection::{HypothesisStatus, ReflectionStore},
    symbiont_context::SymbiontContextStore,
    task_execution::{BoundCodexTask, TaskExecutionQueue, TaskLease, TaskLeaseScope},
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
    #[serde(default)]
    pub task_execution_enabled: bool,
    #[serde(default)]
    pub bound_task: Option<BoundCodexTask>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSnapshot {
    pub codex_task_access: bool,
    pub task_execution_enabled: bool,
    pub pinned_task: Option<BoundCodexTask>,
    pub active_task_lease: Option<TaskLease>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSettingsDraft {
    pub codex_task_access: bool,
    pub task_execution_enabled: bool,
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
    active_task_lease: RwLock<Option<TaskLease>>,
    codex: Arc<Mutex<CodexClient>>,
    continuity: Arc<ContinuityHost>,
    profile: Arc<ProfileStore>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
    task_execution: Arc<TaskExecutionQueue>,
    assets: Arc<AssetStore>,
}

impl CodexBridge {
    pub async fn open(
        path: PathBuf,
        codex: Arc<Mutex<CodexClient>>,
        continuity: Arc<ContinuityHost>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        task_execution: Arc<TaskExecutionQueue>,
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
        let active_task_lease = config
            .bound_task
            .clone()
            .map(|task| TaskLease::new(task, TaskLeaseScope::Pinned));
        let bridge = Self {
            path,
            config: RwLock::new(config),
            active_task_lease: RwLock::new(active_task_lease),
            codex,
            continuity,
            profile,
            context,
            curiosity,
            reflection,
            task_execution,
            assets,
        };
        bridge.persist().await?;
        bridge.suspend_execution().await;
        Ok(bridge)
    }

    pub async fn snapshot(&self) -> BridgeSnapshot {
        let config = self.config.read().await.clone();
        let active_task_lease = if config.codex_task_access {
            self.effective_task_lease(&config).await
        } else {
            None
        };
        BridgeSnapshot {
            codex_task_access: config.codex_task_access,
            task_execution_enabled: config.task_execution_enabled,
            pinned_task: config.bound_task,
            active_task_lease,
        }
    }

    pub async fn update_settings(&self, draft: BridgeSettingsDraft) -> Result<BridgeSnapshot> {
        let mut config = self.config.read().await.clone();
        config.codex_task_access = draft.codex_task_access;
        config.task_execution_enabled = draft.task_execution_enabled && config.codex_task_access;
        let content = toml::to_string_pretty(&config).context("encode Codex bridge config")?;
        persist(&self.path, &content).await?;
        *self.config.write().await = config.clone();
        self.suspend_execution().await;
        Ok(self.snapshot().await)
    }

    pub async fn task_access_enabled(&self) -> bool {
        self.config.read().await.codex_task_access
    }

    pub async fn prompt(&self) -> String {
        let snapshot = self.snapshot().await;
        match (
            &snapshot.active_task_lease,
            snapshot.task_execution_enabled,
        ) {
            (Some(lease), true) => format!(
                "Host Codex bridge: the user selected task `{}` at `{}` for this turn with a `{}` \
                 lease. `delegate_to_selected_task` can spend that lease once. Delegate only \
                 concrete repository work the user asked for or clearly authorized; discussion \
                 alone is not authorization.",
                lease.task.title,
                lease.task.cwd,
                scope_name(lease.scope)
            ),
            (Some(lease), false) => format!(
                "Host Codex bridge: task `{}` is selected read-only. Do not delegate code execution.",
                lease.task.title
            ),
            (None, _) => {
                "Host Codex bridge: no Codex task is selected for this turn. Do not delegate code execution."
                    .to_owned()
            }
        }
    }

    pub async fn list_tasks(&self, limit: u32) -> Result<Vec<CodexTaskSummary>> {
        self.codex.lock().await.list_tasks(limit).await
    }

    pub async fn read_task(&self, thread_id: &str) -> Result<CodexTaskDetail> {
        self.codex.lock().await.read_task(thread_id).await
    }

    pub async fn select_task(
        &self,
        thread_id: &str,
        scope: TaskLeaseScope,
    ) -> Result<BridgeSnapshot> {
        if !self.task_access_enabled().await {
            anyhow::bail!("Codex task access is disabled");
        }
        let detail = self.read_task(thread_id).await?;
        if detail.task.cwd.trim().is_empty() {
            anyhow::bail!("the selected Codex task does not expose a working directory");
        }
        let task = BoundCodexTask {
            thread_id: detail.task.id,
            title: detail.task.title,
            cwd: detail.task.cwd,
            bound_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        };
        if scope == TaskLeaseScope::Pinned {
            let mut config = self.config.read().await.clone();
            config.bound_task = Some(task.clone());
            let content = toml::to_string_pretty(&config).context("encode Codex bridge config")?;
            persist(&self.path, &content).await?;
            *self.config.write().await = config;
        }
        *self.active_task_lease.write().await = Some(TaskLease::new(task, scope));
        self.suspend_execution().await;
        Ok(self.snapshot().await)
    }

    pub async fn bind_task(&self, thread_id: &str) -> Result<BridgeSnapshot> {
        self.select_task(thread_id, TaskLeaseScope::Pinned).await
    }

    pub async fn clear_task_target(&self) -> Result<BridgeSnapshot> {
        let active_scope = self
            .active_task_lease
            .read()
            .await
            .as_ref()
            .map(|lease| lease.scope);
        *self.active_task_lease.write().await = None;
        if active_scope == Some(TaskLeaseScope::Pinned) {
            let mut config = self.config.read().await.clone();
            config.bound_task = None;
            let content = toml::to_string_pretty(&config).context("encode Codex bridge config")?;
            persist(&self.path, &content).await?;
            *self.config.write().await = config;
        }
        self.suspend_execution().await;
        Ok(self.snapshot().await)
    }

    pub async fn unbind_task(&self) -> Result<BridgeSnapshot> {
        let mut config = self.config.read().await.clone();
        config.bound_task = None;
        let content = toml::to_string_pretty(&config).context("encode Codex bridge config")?;
        persist(&self.path, &content).await?;
        *self.config.write().await = config;
        let mut active = self.active_task_lease.write().await;
        if active
            .as_ref()
            .is_some_and(|lease| lease.scope == TaskLeaseScope::Pinned)
        {
            *active = None;
        }
        drop(active);
        self.suspend_execution().await;
        Ok(self.snapshot().await)
    }

    pub async fn begin_interactive_turn(&self, source_revision_id: &str) {
        let config = self.config.read().await.clone();
        let Some(lease) = self.effective_task_lease(&config).await else {
            self.suspend_execution().await;
            return;
        };
        let lease = lease.for_turn(source_revision_id);
        *self.active_task_lease.write().await = Some(lease.clone());
        self.task_execution
            .configure(
                config.codex_task_access && config.task_execution_enabled,
                Some(lease),
            )
            .await;
    }

    pub async fn suspend_execution(&self) {
        self.task_execution.configure(false, None).await;
    }

    pub async fn finish_interactive_turn(&self, source_revision_id: &str) {
        self.suspend_execution().await;
        let should_clear = self
            .active_task_lease
            .read()
            .await
            .as_ref()
            .is_some_and(|lease| {
                lease.scope == TaskLeaseScope::OneShot
                    && lease.source_revision_id.as_deref() == Some(source_revision_id)
            });
        if should_clear {
            *self.active_task_lease.write().await = None;
        }
        let config = self.config.read().await.clone();
        let _ = self.effective_task_lease(&config).await;
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

    async fn effective_task_lease(&self, config: &BridgeConfig) -> Option<TaskLease> {
        let mut active = self.active_task_lease.write().await;
        if active.as_ref().is_some_and(TaskLease::is_expired) {
            *active = None;
        }
        if active.is_none() {
            *active = config
                .bound_task
                .clone()
                .map(|task| TaskLease::new(task, TaskLeaseScope::Pinned));
        }
        active.clone()
    }
}

fn scope_name(scope: TaskLeaseScope) -> &'static str {
    match scope {
        TaskLeaseScope::OneShot => "one_shot",
        TaskLeaseScope::Topic => "topic",
        TaskLeaseScope::Pinned => "pinned",
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
