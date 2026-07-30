use std::{io::ErrorKind, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{SearchFilters, SearchMode, SearchPagesRequest};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{Mutex, RwLock},
};

use crate::{
    codex::{CodexClient, CodexTaskDetail, CodexTaskSummary},
    continuity::ContinuityHost,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeConfig {
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
pub struct BridgeContextPacket {
    pub generated_at: String,
    pub query: Option<String>,
    pub orientation: String,
    pub current_map: Option<String>,
    pub open_loops: Option<String>,
    pub active_hunches: Vec<BridgeSignal>,
    pub working_hypotheses: Vec<BridgeSignal>,
    pub recalled_pages: Vec<BridgeRecall>,
}

pub struct CodexBridge {
    path: PathBuf,
    config: RwLock<BridgeConfig>,
    codex: Arc<Mutex<CodexClient>>,
    continuity: Arc<ContinuityHost>,
    profile: Arc<ProfileStore>,
    context: Arc<SymbiontContextStore>,
    curiosity: Arc<CuriosityStore>,
    reflection: Arc<ReflectionStore>,
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
            codex,
            continuity,
            profile,
            context,
            curiosity,
            reflection,
        };
        bridge.persist().await?;
        Ok(bridge)
    }

    pub async fn config(&self) -> BridgeConfig {
        self.config.read().await.clone()
    }

    pub async fn update_config(&self, config: BridgeConfig) -> Result<BridgeConfig> {
        let content = toml::to_string_pretty(&config).context("encode Codex bridge config")?;
        persist(&self.path, &content).await?;
        *self.config.write().await = config.clone();
        Ok(config)
    }

    pub async fn task_access_enabled(&self) -> bool {
        self.config.read().await.codex_task_access
    }

    pub async fn list_tasks(&self, limit: u32) -> Result<Vec<CodexTaskSummary>> {
        self.codex.lock().await.list_tasks(limit).await
    }

    pub async fn read_task(&self, thread_id: &str) -> Result<CodexTaskDetail> {
        self.codex.lock().await.read_task(thread_id).await
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
        let recalled_pages = if let Some(query) = query.as_deref() {
            self.continuity
                .search(SearchPagesRequest {
                    query: query.to_owned(),
                    scopes: Vec::new(),
                    mode: SearchMode::Text,
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
        })
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
