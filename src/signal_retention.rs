use std::{io::ErrorKind, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock, time::sleep};
use tracing::warn;

use crate::signals::SignalStore;

const DEFAULT_RETENTION_DAYS: u16 = 7;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// User-controlled lifetime for unadopted external conversation inputs.
///
/// This is intentionally independent from the general autonomy schedule: it
/// governs local transient data, whether or not background exploration is
/// currently enabled.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalRetentionConfig {
    #[serde(default = "default_retention_days")]
    pub retention_days: u16,
}

impl Default for SignalRetentionConfig {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_RETENTION_DAYS,
        }
    }
}

pub struct SignalRetentionStore {
    path: PathBuf,
    config: RwLock<SignalRetentionConfig>,
}

impl SignalRetentionStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let config = match fs::read_to_string(&path).await {
            Ok(content) => {
                toml::from_str::<SignalRetentionConfig>(&content).with_context(|| {
                    format!("parse external-input retention config {}", path.display())
                })?
            }
            Err(error) if error.kind() == ErrorKind::NotFound => SignalRetentionConfig::default(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read external-input retention config {}", path.display())
                });
            }
        };
        validate(config)?;
        let store = Self {
            path,
            config: RwLock::new(config),
        };
        store.persist().await?;
        Ok(store)
    }

    pub async fn snapshot(&self) -> SignalRetentionConfig {
        *self.config.read().await
    }

    pub async fn update(&self, config: SignalRetentionConfig) -> Result<SignalRetentionConfig> {
        validate(config)?;
        let content =
            toml::to_string_pretty(&config).context("encode external-input retention config")?;
        persist(&self.path, &content).await?;
        *self.config.write().await = config;
        Ok(config)
    }

    async fn persist(&self) -> Result<()> {
        let content = toml::to_string_pretty(&*self.config.read().await)
            .context("encode external-input retention config")?;
        persist(&self.path, &content).await
    }
}

pub fn start_cleanup(signals: Arc<SignalStore>, retention: Arc<SignalRetentionStore>) {
    tokio::spawn(async move {
        loop {
            let config = retention.snapshot().await;
            match signals.expire_unadopted(config.retention_days).await {
                Ok(summary) if summary.changed() => {
                    tracing::info!(
                        target: crate::runtime_log::TARGET,
                        event = "external_input_expired",
                        expired_external_inputs = summary.expired_external_inputs,
                        expired_attacker_challenges = summary.expired_attacker_challenges,
                        retention_days = config.retention_days,
                        "expired unadopted external inputs"
                    );
                }
                Ok(_) => {}
                Err(error) => warn!(
                    target: crate::runtime_log::TARGET,
                    event = "external_input_expiry_failed",
                    %error,
                    "could not expire unadopted external inputs"
                ),
            }
            sleep(CLEANUP_INTERVAL).await;
        }
    });
}

const fn default_retention_days() -> u16 {
    DEFAULT_RETENTION_DAYS
}

fn validate(config: SignalRetentionConfig) -> Result<()> {
    if matches!(config.retention_days, 0 | 3 | 7 | 14 | 30) {
        Ok(())
    } else {
        anyhow::bail!("external-input retention must be off, 3, 7, 14, or 30 days")
    }
}

async fn persist(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "create external-input retention directory {}",
                parent.display()
            )
        })?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content).await.with_context(|| {
        format!(
            "write external-input retention config {}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace external-input retention config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{SignalRetentionConfig, SignalRetentionStore};

    #[tokio::test]
    async fn retention_accepts_the_explicit_lifetime_choices() {
        let path = std::env::temp_dir().join(format!(
            "symbiont-signal-retention-{}.toml",
            std::process::id()
        ));
        let store = SignalRetentionStore::open(path.clone()).await.unwrap();
        assert_eq!(store.snapshot().await.retention_days, 7);

        for retention_days in [0, 3, 7, 14, 30] {
            store
                .update(SignalRetentionConfig { retention_days })
                .await
                .unwrap();
        }
        assert!(
            store
                .update(SignalRetentionConfig { retention_days: 5 })
                .await
                .is_err()
        );
        let _ = tokio::fs::remove_file(path).await;
    }
}
