use std::{collections::HashSet, io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{fs, sync::RwLock};
use tracing::warn;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffortInfo>,
    pub service_tiers: Vec<ServiceTierInfo>,
    pub input_modalities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortInfo {
    pub reasoning_effort: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServiceTierInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeLane {
    Observe,
    Conversation,
    Investigate,
    Critical,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    Fixed,
    BoundedAuto,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneConfig {
    pub model: String,
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LaneConfigs {
    pub observe: LaneConfig,
    pub conversation: LaneConfig,
    pub investigate: LaneConfig,
    pub critical: LaneConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeConfig {
    pub routing: RoutingMode,
    pub show_model: bool,
    pub lanes: LaneConfigs,
}

pub struct ComputeStore {
    path: PathBuf,
    catalog: Vec<ModelInfo>,
    config: RwLock<ComputeConfig>,
}

impl ModelInfo {
    pub fn from_app_server(value: &Value) -> Result<Self> {
        let supported_reasoning_efforts = value
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|effort| ReasoningEffortInfo {
                reasoning_effort: text(effort, "reasoningEffort").unwrap_or_default(),
                description: text(effort, "description").unwrap_or_default(),
            })
            .filter(|effort| !effort.reasoning_effort.is_empty())
            .collect();
        let service_tiers = value
            .get("serviceTiers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|tier| ServiceTierInfo {
                id: text(tier, "id").unwrap_or_default(),
                name: text(tier, "name").unwrap_or_default(),
                description: text(tier, "description").unwrap_or_default(),
            })
            .filter(|tier| !tier.id.is_empty())
            .collect();

        Ok(Self {
            id: required_text(value, "id")?,
            model: required_text(value, "model")?,
            display_name: required_text(value, "displayName")?,
            description: required_text(value, "description")?,
            is_default: value
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            default_reasoning_effort: required_text(value, "defaultReasoningEffort")?,
            supported_reasoning_efforts,
            service_tiers,
            input_modalities: value
                .get("inputModalities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        })
    }
}

impl ComputeLane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Conversation => "conversation",
            Self::Investigate => "investigate",
            Self::Critical => "critical",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observe" => Some(Self::Observe),
            "conversation" | "auto" => Some(Self::Conversation),
            "investigate" | "deep" => Some(Self::Investigate),
            "critical" | "max" => Some(Self::Critical),
            _ => None,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Observe => 0,
            Self::Conversation => 1,
            Self::Investigate => 2,
            Self::Critical => 3,
        }
    }
}

impl ComputeConfig {
    pub fn lane(&self, lane: ComputeLane) -> &LaneConfig {
        match lane {
            ComputeLane::Observe => &self.lanes.observe,
            ComputeLane::Conversation => &self.lanes.conversation,
            ComputeLane::Investigate => &self.lanes.investigate,
            ComputeLane::Critical => &self.lanes.critical,
        }
    }

    pub fn allows_escalation(&self, from: ComputeLane, to: ComputeLane) -> bool {
        self.routing == RoutingMode::BoundedAuto && to.rank() > from.rank()
    }

    fn defaults(catalog: &[ModelInfo]) -> Result<Self> {
        let fallback = catalog
            .iter()
            .find(|model| model.is_default)
            .or_else(|| catalog.first())
            .context("Codex returned an empty model catalog")?;
        let observe =
            preferred_model(catalog, &["gpt-5.6-luna", "gpt-5.4-mini"]).unwrap_or(fallback);
        let conversation =
            preferred_model(catalog, &["gpt-5.6-terra", "gpt-5.4"]).unwrap_or(fallback);
        let deep = preferred_model(catalog, &["gpt-5.6-sol", "gpt-5.5"]).unwrap_or(fallback);

        Ok(Self {
            routing: RoutingMode::BoundedAuto,
            show_model: true,
            lanes: LaneConfigs {
                observe: lane_default(observe, &["medium", "low"]),
                conversation: lane_default(conversation, &["medium", "low"]),
                investigate: lane_default(deep, &["high", "xhigh", "medium"]),
                critical: lane_default(deep, &["xhigh", "high", "medium"]),
            },
        })
    }
}

impl ComputeStore {
    pub async fn open(path: PathBuf, catalog: Vec<ModelInfo>) -> Result<Self> {
        if catalog.is_empty() {
            anyhow::bail!("Codex returned no available models");
        }
        let defaults = ComputeConfig::defaults(&catalog)?;
        let config = match fs::read_to_string(&path).await {
            Ok(content) => match toml::from_str::<ComputeConfig>(&content) {
                Ok(config) if validate(&config, &catalog).is_ok() => config,
                Ok(_) => {
                    warn!(
                        "compute configuration references unavailable model settings; using defaults"
                    );
                    defaults
                }
                Err(error) => {
                    warn!(%error, "compute configuration could not be parsed; using defaults");
                    defaults
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => defaults,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read compute config {}", path.display()));
            }
        };

        persist(&path, &config).await?;
        Ok(Self {
            path,
            catalog,
            config: RwLock::new(config),
        })
    }

    pub fn catalog(&self) -> &[ModelInfo] {
        &self.catalog
    }

    pub async fn snapshot(&self) -> ComputeConfig {
        self.config.read().await.clone()
    }

    pub async fn update(&self, config: ComputeConfig) -> Result<ComputeConfig> {
        validate(&config, &self.catalog)?;
        persist(&self.path, &config).await?;
        *self.config.write().await = config.clone();
        Ok(config)
    }
}

fn validate(config: &ComputeConfig, catalog: &[ModelInfo]) -> Result<()> {
    for lane in [
        ComputeLane::Observe,
        ComputeLane::Conversation,
        ComputeLane::Investigate,
        ComputeLane::Critical,
    ] {
        let settings = config.lane(lane);
        let model = catalog
            .iter()
            .find(|model| model.model == settings.model || model.id == settings.model)
            .with_context(|| {
                format!(
                    "{} lane references unavailable model {}",
                    lane.as_str(),
                    settings.model
                )
            })?;
        let efforts: HashSet<&str> = model
            .supported_reasoning_efforts
            .iter()
            .map(|effort| effort.reasoning_effort.as_str())
            .collect();
        if !efforts.contains(settings.effort.as_str()) {
            anyhow::bail!(
                "{} does not support reasoning effort {}",
                model.display_name,
                settings.effort
            );
        }
        if let Some(service_tier) = settings.service_tier.as_deref()
            && !model
                .service_tiers
                .iter()
                .any(|tier| tier.id == service_tier)
        {
            anyhow::bail!(
                "{} does not support service tier {}",
                model.display_name,
                service_tier
            );
        }
    }
    Ok(())
}

async fn persist(path: &PathBuf, config: &ComputeConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create compute config directory {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(config).context("encode compute configuration")?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write compute config {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace compute config {}", path.display()))
}

fn lane_default(model: &ModelInfo, preferred_efforts: &[&str]) -> LaneConfig {
    let effort = preferred_efforts
        .iter()
        .find(|preferred| {
            model
                .supported_reasoning_efforts
                .iter()
                .any(|option| option.reasoning_effort == **preferred)
        })
        .map(|effort| (*effort).to_owned())
        .unwrap_or_else(|| model.default_reasoning_effort.clone());
    LaneConfig {
        model: model.model.clone(),
        effort,
        service_tier: None,
    }
}

fn preferred_model<'a>(catalog: &'a [ModelInfo], preferred: &[&str]) -> Option<&'a ModelInfo> {
    preferred
        .iter()
        .find_map(|slug| catalog.iter().find(|model| model.model == *slug))
}

fn text(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn required_text(value: &Value, field: &str) -> Result<String> {
    text(value, field).with_context(|| format!("model catalog entry omitted {field}"))
}

#[cfg(test)]
#[path = "compute/tests.rs"]
mod tests;
