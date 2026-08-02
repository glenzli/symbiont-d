use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{RwLock, watch},
};

const DEFAULT_DAILY_TOKEN_LIMIT: u64 = 100_000;
const MAX_DAILY_TOKEN_LIMIT: u64 = 100_000_000;
const DEFAULT_DAILY_NOTE_LIMIT: u8 = 2;
const MAX_DAILY_OUTREACH_LIMIT: u8 = 20;
const DEFAULT_ATTENTION_POSTURE: &str = "我不想每天自己刷新闻。除了会改变当前决策的信号，也请留意可信、新鲜，并且和我的长期问题、项目或思考方式真正相关的外部变化；它不必立刻导出行动。若要切换方向，请明确说出来，不要假装在接续上一段对话。";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomyConfig {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub daily_interrupt_limit: u8,
    #[serde(default = "default_daily_note_limit")]
    pub daily_note_limit: u8,
    #[serde(default = "default_daily_token_limit")]
    pub daily_token_limit: u64,
    pub quiet_hours: QuietHours,
    #[serde(default = "default_attention_posture")]
    pub attention_posture: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuietHours {
    pub enabled: bool,
    pub start: String,
    pub end: String,
}

impl AutonomyConfig {
    pub fn attention_context(&self) -> String {
        format!(
            "<attention-posture>\n{}\nDaily intervention limit: {}. Daily note limit: {}.\n</attention-posture>",
            self.attention_posture.trim(),
            self.daily_interrupt_limit,
            self.daily_note_limit,
        )
    }
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
            daily_note_limit: DEFAULT_DAILY_NOTE_LIMIT,
            daily_token_limit: DEFAULT_DAILY_TOKEN_LIMIT,
            quiet_hours: QuietHours {
                enabled: true,
                start: "23:00".to_owned(),
                end: "08:00".to_owned(),
            },
            attention_posture: default_attention_posture(),
        }
    }
}

fn validate(config: &AutonomyConfig) -> Result<()> {
    if !(30..=10_080).contains(&config.interval_minutes) {
        anyhow::bail!("exploration interval must be between 30 minutes and 7 days");
    }
    if config.daily_interrupt_limit > MAX_DAILY_OUTREACH_LIMIT {
        anyhow::bail!("daily interruption limit cannot exceed 20");
    }
    if config.daily_note_limit > MAX_DAILY_OUTREACH_LIMIT {
        anyhow::bail!("daily note limit cannot exceed 20");
    }
    if config.daily_token_limit > MAX_DAILY_TOKEN_LIMIT {
        anyhow::bail!("daily token limit cannot exceed {MAX_DAILY_TOKEN_LIMIT}");
    }
    validate_time(&config.quiet_hours.start)?;
    validate_time(&config.quiet_hours.end)?;
    if config.attention_posture.trim().is_empty() {
        anyhow::bail!("attention posture cannot be empty");
    }
    if config.attention_posture.chars().count() > 2_000 {
        anyhow::bail!("attention posture cannot exceed 2000 characters");
    }
    Ok(())
}

const fn default_daily_token_limit() -> u64 {
    DEFAULT_DAILY_TOKEN_LIMIT
}

const fn default_daily_note_limit() -> u8 {
    DEFAULT_DAILY_NOTE_LIMIT
}

fn default_attention_posture() -> String {
    DEFAULT_ATTENTION_POSTURE.to_owned()
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
