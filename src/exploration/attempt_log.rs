use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Mutex};

const RETAINED_ATTEMPTS: usize = 32;

/// A scheduled or manual pass that deliberately produced no exploration
/// trace. This is operational history, not memory and not a model result.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationSkippedAttempt {
    pub at: String,
    pub trigger: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExplorationAttemptDocument {
    skipped: Vec<ExplorationSkippedAttempt>,
}

/// Owns the small, durable operational log for passes that did not actually
/// reach a provider. Invocation history remains the source of truth whenever
/// a model call occurred.
pub struct ExplorationAttemptStore {
    path: PathBuf,
    document: Mutex<ExplorationAttemptDocument>,
}

impl ExplorationAttemptStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let mut document = match fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content)
                .with_context(|| format!("parse exploration attempt log {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                ExplorationAttemptDocument::default()
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read exploration attempt log {}", path.display()));
            }
        };
        trim_attempts(&mut document.skipped);
        let store = Self {
            path,
            document: Mutex::new(document),
        };
        let document = store.document.lock().await;
        store.persist_locked(&document).await?;
        drop(document);
        Ok(store)
    }

    pub async fn record(
        &self,
        trigger: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<ExplorationSkippedAttempt> {
        let mut document = self.document.lock().await;
        let before = document.clone();
        let entry = ExplorationSkippedAttempt {
            at: timestamp(Utc::now()),
            trigger: trigger.into(),
            reason: reason.into(),
        };
        document.skipped.push(entry.clone());
        trim_attempts(&mut document.skipped);
        if let Err(error) = self.persist_locked(&document).await {
            *document = before;
            return Err(error);
        }
        Ok(entry)
    }

    pub async fn recent(&self, limit: usize) -> Vec<ExplorationSkippedAttempt> {
        let document = self.document.lock().await;
        document.skipped.iter().rev().take(limit).cloned().collect()
    }

    pub async fn latest(&self) -> Option<ExplorationSkippedAttempt> {
        let document = self.document.lock().await;
        document.skipped.last().cloned()
    }

    pub async fn remove_reason(&self, reason: &str) -> Result<bool> {
        let mut document = self.document.lock().await;
        let before = document.clone();
        document.skipped.retain(|attempt| attempt.reason != reason);
        if document.skipped.len() == before.skipped.len() {
            return Ok(false);
        }
        if let Err(error) = self.persist_locked(&document).await {
            *document = before;
            return Err(error);
        }
        Ok(true)
    }

    async fn persist_locked(&self, document: &ExplorationAttemptDocument) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("create exploration attempt directory {}", parent.display())
            })?;
        }
        let content =
            serde_json::to_string_pretty(document).context("encode exploration attempt log")?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write exploration attempt log {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace exploration attempt log {}", self.path.display()))
    }
}

fn trim_attempts(attempts: &mut Vec<ExplorationSkippedAttempt>) {
    if attempts.len() > RETAINED_ATTEMPTS {
        attempts.drain(..attempts.len() - RETAINED_ATTEMPTS);
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::ExplorationAttemptStore;

    #[tokio::test]
    async fn skipped_attempts_survive_restart_in_reverse_recent_order() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-exploration-attempt-{nonce}.json"));
        let store = ExplorationAttemptStore::open(path.clone())
            .await
            .expect("open log");
        store
            .record("scheduled", "no_input_channel")
            .await
            .expect("record first skip");
        store
            .record("manual", "channel_failed")
            .await
            .expect("record second skip");
        drop(store);

        let reopened = ExplorationAttemptStore::open(path.clone())
            .await
            .expect("reopen log");
        let recent = reopened.recent(2).await;
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].reason, "channel_failed");
        assert_eq!(recent[1].reason, "no_input_channel");
        assert_eq!(reopened.latest().await.unwrap().trigger, "manual");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn can_remove_invalidated_skip_reasons() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("symbiont-exploration-attempt-prune-{nonce}.json"));
        let store = ExplorationAttemptStore::open(path.clone())
            .await
            .expect("open log");
        store
            .record("scheduled", "no_input_channel")
            .await
            .expect("record");
        store
            .record("scheduled", "channel_failed")
            .await
            .expect("record");
        assert!(
            store
                .remove_reason("no_input_channel")
                .await
                .expect("prune")
        );
        assert_eq!(store.recent(4).await.len(), 1);
        assert_eq!(store.latest().await.unwrap().reason, "channel_failed");
        let _ = std::fs::remove_file(path);
    }
}
