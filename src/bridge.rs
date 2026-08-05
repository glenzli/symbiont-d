mod recall;

#[allow(unused_imports)]
pub use recall::{
    BridgeContextPacket, BridgeEvidence, BridgeExpandRequest, BridgeImage, BridgeRecall,
    BridgeRecallBundle, BridgeRecallDepth, BridgeRecallExpansion, BridgeRecallRequest,
    BridgeRelatedContext, BridgeSignal, BridgeSupportingPage, BridgeUnderstanding,
};

use std::{io::ErrorKind, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::{
    asset::AssetStore,
    codex::{CodexTaskDetail, CodexTaskSources, CodexTaskSummary},
    continuity::ContinuityHost,
    curiosity::CuriosityStore,
    profile::ProfileStore,
    reflection::ReflectionStore,
    symbiont_context::SymbiontContextStore,
};
use recall::RecallService;

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

pub struct CodexBridge {
    path: PathBuf,
    config: RwLock<BridgeConfig>,
    task_sources: Arc<CodexTaskSources>,
    recall: RecallService,
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
            recall: RecallService::new(continuity, profile, context, curiosity, reflection, assets),
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
        self.recall.context_packet(query).await
    }

    pub async fn recall(&self, request: BridgeRecallRequest) -> Result<BridgeRecallBundle> {
        self.recall.recall(request).await
    }

    pub async fn expand(&self, request: BridgeExpandRequest) -> Result<BridgeRecallExpansion> {
        self.recall.expand(request).await
    }

    async fn persist(&self) -> Result<()> {
        let content = toml::to_string_pretty(&*self.config.read().await)
            .context("encode Codex bridge config")?;
        persist(&self.path, &content).await
    }
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
