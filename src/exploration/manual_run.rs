use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Mutex};

const RETAINED_RECEIPTS: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualExplorationStatus {
    Queued,
    Exploring,
    Messaged,
    Silent,
    Failed,
}

impl ManualExplorationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Messaged | Self::Silent | Self::Failed)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualExplorationRun {
    pub id: String,
    pub status: ManualExplorationStatus,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub attempts: u32,
    pub reason: Option<String>,
    pub outcome: Option<String>,
    pub result_revision_id: Option<String>,
    #[serde(default)]
    pub presented_at: Option<String>,
}

impl ManualExplorationRun {
    fn queued(id: String, requested_at: String) -> Self {
        Self {
            id,
            status: ManualExplorationStatus::Queued,
            requested_at,
            started_at: None,
            completed_at: None,
            attempts: 0,
            reason: None,
            outcome: None,
            result_revision_id: None,
            presented_at: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ManualExplorationProjection {
    pub latest: Option<ManualExplorationRun>,
    pub unpresented: Vec<ManualExplorationRun>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManualExplorationDocument {
    receipts: Vec<ManualExplorationRun>,
}

pub struct ManualExplorationStore {
    path: PathBuf,
    document: Mutex<ManualExplorationDocument>,
}

impl ManualExplorationStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let mut document = match fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content)
                .with_context(|| format!("parse manual exploration receipts {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                ManualExplorationDocument::default()
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read manual exploration receipts {}", path.display())
                });
            }
        };
        let recovered_at = timestamp(Utc::now());
        let recovered = document
            .receipts
            .iter_mut()
            .filter(|run| !run.status.is_terminal())
            .map(|run| {
                run.status = ManualExplorationStatus::Failed;
                run.completed_at = Some(recovered_at.clone());
                run.reason = Some("service_restarted".to_owned());
                run.outcome = Some("failed".to_owned());
                run.presented_at = None;
                run.id.clone()
            })
            .collect::<Vec<_>>();
        trim_receipts(&mut document.receipts);
        let store = Self {
            path,
            document: Mutex::new(document),
        };
        {
            let document = store.document.lock().await;
            store.persist_locked(&document).await?;
        }
        for request_id in recovered {
            tracing::warn!(
                target: crate::runtime_log::TARGET,
                event = "manual_exploration_recovered",
                request_id,
                "unfinished manual exploration was recovered as failed"
            );
        }
        Ok(store)
    }

    pub async fn projection(&self) -> ManualExplorationProjection {
        let document = self.document.lock().await;
        ManualExplorationProjection {
            latest: document.receipts.last().cloned(),
            unpresented: document
                .receipts
                .iter()
                .filter(|run| run.status.is_terminal() && run.presented_at.is_none())
                .cloned()
                .collect(),
        }
    }

    pub async fn accept(
        &self,
        id: String,
        requested_at: String,
    ) -> Result<Option<ManualExplorationRun>> {
        let mut document = self.document.lock().await;
        if document
            .receipts
            .iter()
            .any(|run| !run.status.is_terminal())
        {
            return Ok(None);
        }
        let before = document.clone();
        let run = ManualExplorationRun::queued(id, requested_at);
        document.receipts.push(run.clone());
        trim_receipts(&mut document.receipts);
        if let Err(error) = self.persist_locked(&document).await {
            *document = before;
            return Err(error);
        }
        Ok(Some(run))
    }

    pub async fn mark_exploring(
        &self,
        id: &str,
        started_at: String,
    ) -> Result<Option<ManualExplorationRun>> {
        self.update(id, |run| {
            run.status = ManualExplorationStatus::Exploring;
            run.started_at.get_or_insert(started_at);
            run.attempts = run.attempts.saturating_add(1);
            run.reason = None;
        })
        .await
    }

    pub async fn requeue(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<Option<ManualExplorationRun>> {
        self.update(id, |run| {
            run.status = ManualExplorationStatus::Queued;
            run.reason = reason.map(str::to_owned);
        })
        .await
    }

    pub async fn complete(
        &self,
        id: &str,
        status: ManualExplorationStatus,
        completed_at: String,
        outcome: String,
        result_revision_id: Option<String>,
    ) -> Result<Option<ManualExplorationRun>> {
        if !status.is_terminal() {
            anyhow::bail!("manual exploration completion requires a terminal status");
        }
        self.update(id, |run| {
            run.status = status;
            run.completed_at = Some(completed_at);
            run.reason = None;
            run.outcome = Some(outcome);
            run.result_revision_id = result_revision_id;
            run.presented_at = None;
        })
        .await
    }

    pub async fn fail(&self, id: &str, reason: &str) -> Result<Option<ManualExplorationRun>> {
        self.update(id, |run| {
            run.status = ManualExplorationStatus::Failed;
            run.completed_at = Some(timestamp(Utc::now()));
            run.reason = Some(reason.to_owned());
            run.outcome = Some("failed".to_owned());
            run.presented_at = None;
        })
        .await
    }

    pub async fn acknowledge(&self, id: &str) -> Result<Option<ManualExplorationRun>> {
        self.update(id, |run| {
            if run.status.is_terminal() && run.presented_at.is_none() {
                run.presented_at = Some(timestamp(Utc::now()));
            }
        })
        .await
    }

    async fn update(
        &self,
        id: &str,
        update: impl FnOnce(&mut ManualExplorationRun),
    ) -> Result<Option<ManualExplorationRun>> {
        let mut document = self.document.lock().await;
        let Some(index) = document.receipts.iter().position(|run| run.id == id) else {
            return Ok(None);
        };
        let before = document.clone();
        update(&mut document.receipts[index]);
        let updated = document.receipts[index].clone();
        if let Err(error) = self.persist_locked(&document).await {
            *document = before;
            return Err(error);
        }
        Ok(Some(updated))
    }

    async fn persist_locked(&self, document: &ManualExplorationDocument) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "create manual exploration receipt directory {}",
                    parent.display()
                )
            })?;
        }
        let content =
            serde_json::to_string_pretty(document).context("encode manual exploration receipts")?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content).await.with_context(|| {
            format!("write manual exploration receipts {}", temporary.display())
        })?;
        fs::rename(&temporary, &self.path).await.with_context(|| {
            format!(
                "replace manual exploration receipts {}",
                self.path.display()
            )
        })
    }
}

fn trim_receipts(receipts: &mut Vec<ManualExplorationRun>) {
    if receipts.len() > RETAINED_RECEIPTS {
        receipts.drain(..receipts.len() - RETAINED_RECEIPTS);
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ManualExplorationStatus, ManualExplorationStore};

    fn unique_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "symbiont-manual-exploration-{name}-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    #[tokio::test]
    async fn terminal_receipt_survives_restart_until_acknowledged() {
        let path = unique_path("receipt");
        let store = ManualExplorationStore::open(path.clone())
            .await
            .expect("open store");
        store
            .accept("explore_1".to_owned(), "2026-08-06T00:00:00Z".to_owned())
            .await
            .expect("accept")
            .expect("new run");
        store
            .mark_exploring("explore_1", "2026-08-06T00:00:01Z".to_owned())
            .await
            .expect("start");
        store
            .requeue("explore_1", Some("newer_user_input"))
            .await
            .expect("requeue");
        store
            .mark_exploring("explore_1", "2026-08-06T00:00:02Z".to_owned())
            .await
            .expect("restart");
        store
            .complete(
                "explore_1",
                ManualExplorationStatus::Silent,
                "2026-08-06T00:00:03Z".to_owned(),
                "silent".to_owned(),
                None,
            )
            .await
            .expect("complete");
        drop(store);

        let reopened = ManualExplorationStore::open(path.clone())
            .await
            .expect("reopen store");
        let projection = reopened.projection().await;
        assert_eq!(projection.unpresented.len(), 1);
        assert_eq!(projection.unpresented[0].attempts, 2);
        reopened
            .acknowledge("explore_1")
            .await
            .expect("acknowledge");
        drop(reopened);

        let acknowledged = ManualExplorationStore::open(path.clone())
            .await
            .expect("reopen acknowledged store");
        assert!(acknowledged.projection().await.unpresented.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn unfinished_run_becomes_an_unpresented_failure_after_restart() {
        let path = unique_path("recovery");
        let store = ManualExplorationStore::open(path.clone())
            .await
            .expect("open store");
        store
            .accept("explore_2".to_owned(), "2026-08-06T00:00:00Z".to_owned())
            .await
            .expect("accept");
        store
            .mark_exploring("explore_2", "2026-08-06T00:00:01Z".to_owned())
            .await
            .expect("start");
        drop(store);

        let reopened = ManualExplorationStore::open(path.clone())
            .await
            .expect("reopen store");
        let projection = reopened.projection().await;
        let recovered = projection.latest.expect("recovered run");
        assert_eq!(recovered.status, ManualExplorationStatus::Failed);
        assert_eq!(recovered.reason.as_deref(), Some("service_restarted"));
        assert_eq!(projection.unpresented.len(), 1);
        let _ = std::fs::remove_file(path);
    }
}
