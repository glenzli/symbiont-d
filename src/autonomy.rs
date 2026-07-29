use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{RwLock, watch},
};

const DEFAULT_DAILY_TOKEN_LIMIT: u64 = 100_000;
const MAX_DAILY_TOKEN_LIMIT: u64 = 100_000_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomyConfig {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub daily_interrupt_limit: u8,
    #[serde(default = "default_daily_token_limit")]
    pub daily_token_limit: u64,
    pub quiet_hours: QuietHours,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuietHours {
    pub enabled: bool,
    pub start: String,
    pub end: String,
}

pub struct AutonomyStore {
    path: PathBuf,
    config: RwLock<AutonomyConfig>,
    updates: watch::Sender<AutonomyConfig>,
}

impl AutonomyStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let config = match fs::read_to_string(&path).await {
            Ok(content) => toml::from_str::<AutonomyConfig>(&content)
                .with_context(|| format!("parse autonomy config {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => AutonomyConfig::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read autonomy config {}", path.display()));
            }
        };
        validate(&config)?;
        let (updates, _) = watch::channel(config.clone());
        let store = Self {
            path,
            config: RwLock::new(config),
            updates,
        };
        store.persist().await?;
        Ok(store)
    }

    pub async fn snapshot(&self) -> AutonomyConfig {
        self.config.read().await.clone()
    }

    pub async fn update(&self, config: AutonomyConfig) -> Result<AutonomyConfig> {
        validate(&config)?;
        let content = toml::to_string_pretty(&config).context("encode autonomy configuration")?;
        persist(&self.path, &content).await?;
        *self.config.write().await = config.clone();
        self.updates.send_replace(config.clone());
        Ok(config)
    }

    pub fn subscribe(&self) -> watch::Receiver<AutonomyConfig> {
        self.updates.subscribe()
    }

    pub async fn permitted(&self, initialized: bool) -> bool {
        initialized && self.config.read().await.enabled
    }

    async fn persist(&self) -> Result<()> {
        let content = toml::to_string_pretty(&*self.config.read().await)
            .context("encode autonomy configuration")?;
        persist(&self.path, &content).await
    }
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 360,
            daily_interrupt_limit: 2,
            daily_token_limit: DEFAULT_DAILY_TOKEN_LIMIT,
            quiet_hours: QuietHours {
                enabled: true,
                start: "23:00".to_owned(),
                end: "08:00".to_owned(),
            },
        }
    }
}

fn validate(config: &AutonomyConfig) -> Result<()> {
    if !(30..=10_080).contains(&config.interval_minutes) {
        anyhow::bail!("exploration interval must be between 30 minutes and 7 days");
    }
    if config.daily_interrupt_limit > 20 {
        anyhow::bail!("daily interruption limit cannot exceed 20");
    }
    if config.daily_token_limit > MAX_DAILY_TOKEN_LIMIT {
        anyhow::bail!("daily token limit cannot exceed {MAX_DAILY_TOKEN_LIMIT}");
    }
    validate_time(&config.quiet_hours.start)?;
    validate_time(&config.quiet_hours.end)
}

const fn default_daily_token_limit() -> u64 {
    DEFAULT_DAILY_TOKEN_LIMIT
}

fn validate_time(value: &str) -> Result<()> {
    let Some((hour, minute)) = value.split_once(':') else {
        anyhow::bail!("quiet-hour time must use HH:MM");
    };
    let hour: u8 = hour.parse().context("parse quiet-hour hour")?;
    let minute: u8 = minute.parse().context("parse quiet-hour minute")?;
    if hour > 23 || minute > 59 {
        anyhow::bail!("quiet-hour time is outside the 24-hour clock");
    }
    Ok(())
}

async fn persist(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create autonomy directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write autonomy config {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace autonomy config {}", path.display()))
}

#[cfg(test)]
#[path = "autonomy/tests.rs"]
mod tests;
