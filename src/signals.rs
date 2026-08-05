use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::sensing::{InputRoleSnapshot, SensingCandidate, SensingSource, SensingSourceClass};

const RETENTION_DAYS: i64 = 30;
const MAX_RETAINED_SIGNALS: usize = 100;

/// A visible but non-durable input from an auxiliary model role.
///
/// Signals deliberately live outside PCP. They become durable source material only
/// when the user explicitly replies to one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SignalEvent {
    pub id: String,
    pub candidate_id: String,
    pub actor: InputRoleSnapshot,
    pub content: String,
    pub title: String,
    pub summary: String,
    pub sources: Vec<SensingSource>,
    pub source_class: SensingSourceClass,
    #[serde(default)]
    pub event_at: Option<String>,
    pub observed_at: String,
    pub review_reason: String,
    #[serde(default)]
    pub promoted_revision_id: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Default, Deserialize, Serialize)]
struct SignalDocument {
    #[serde(default)]
    signals: Vec<SignalEvent>,
}

pub struct SignalStore {
    path: PathBuf,
    document: RwLock<SignalDocument>,
}

impl SignalStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let mut document = match fs::read_to_string(&path).await {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(document) => document,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "discarding unreadable local input signal stream");
                    SignalDocument::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SignalDocument::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read input signal stream {}", path.display()));
            }
        };
        let changed = prune(&mut document, Utc::now());
        let store = Self {
            path,
            document: RwLock::new(document),
        };
        if changed {
            store.persist().await?;
        }
        Ok(store)
    }

    pub async fn publish(
        &self,
        candidate: &SensingCandidate,
        review_reason: String,
    ) -> Result<SignalEvent> {
        let now = Utc::now();
        let mut document = self.document.write().await;
        if let Some(existing) = document
            .signals
            .iter()
            .find(|signal| signal.candidate_id == candidate.id)
            .cloned()
        {
            return Ok(existing);
        }
        let sequence = document.signals.len();
        let event = SignalEvent {
            id: format!("signal_{}_{}", now.timestamp_millis(), sequence),
            candidate_id: candidate.id.clone(),
            actor: candidate.actor.clone(),
            content: candidate.proposed_input.clone(),
            title: candidate.title.clone(),
            summary: candidate.summary.clone(),
            sources: candidate.sources.clone(),
            source_class: candidate.source_class,
            event_at: candidate.event_at.clone(),
            observed_at: timestamp(now),
            review_reason: review_reason.trim().to_owned(),
            promoted_revision_id: None,
            hidden: false,
        };
        document.signals.push(event.clone());
        prune(&mut document, now);
        drop(document);
        self.persist().await?;
        Ok(event)
    }

    pub async fn visible(&self, limit: usize) -> Result<Vec<SignalEvent>> {
        let now = Utc::now();
        let mut document = self.document.write().await;
        let changed = prune(&mut document, now);
        let signals = document
            .signals
            .iter()
            .filter(|signal| !signal.hidden)
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        drop(document);
        if changed {
            self.persist().await?;
        }
        Ok(signals.into_iter().rev().collect())
    }

    pub async fn get(&self, id: &str) -> Result<Option<SignalEvent>> {
        let document = self.document.read().await;
        Ok(document
            .signals
            .iter()
            .find(|signal| signal.id == id)
            .cloned())
    }

    pub async fn mark_promoted(
        &self,
        id: &str,
        revision_id: String,
    ) -> Result<Option<SignalEvent>> {
        let mut document = self.document.write().await;
        let event = document
            .signals
            .iter_mut()
            .find(|signal| signal.id == id)
            .map(|signal| {
                if signal.promoted_revision_id.is_none() {
                    signal.promoted_revision_id = Some(revision_id);
                }
                signal.clone()
            });
        drop(document);
        if event.is_some() {
            self.persist().await?;
        }
        Ok(event)
    }

    async fn persist(&self) -> Result<()> {
        let content = {
            let document = self.document.read().await;
            serde_json::to_string_pretty(&*document).context("encode input signal stream")?
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create input signal directory {}", parent.display()))?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write input signal stream {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace input signal stream {}", self.path.display()))
    }
}

fn prune(document: &mut SignalDocument, now: DateTime<Utc>) -> bool {
    let before = document.signals.len();
    let oldest = now - Duration::days(RETENTION_DAYS);
    document.signals.retain(|signal| {
        DateTime::parse_from_rfc3339(&signal.observed_at)
            .map(|observed_at| observed_at.with_timezone(&Utc) >= oldest)
            .unwrap_or(false)
    });
    if document.signals.len() > MAX_RETAINED_SIGNALS {
        let drop_count = document.signals.len() - MAX_RETAINED_SIGNALS;
        document.signals.drain(0..drop_count);
    }
    before != document.signals.len()
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::SignalStore;
    use crate::sensing::{InputRoleSnapshot, SensingCandidate, SensingSource, SensingSourceClass};

    fn candidate(id: &str) -> SensingCandidate {
        SensingCandidate {
            id: id.to_owned(),
            title: "A signal".to_owned(),
            summary: "A compact summary".to_owned(),
            proposed_input: "A model input.".to_owned(),
            event_at: None,
            source_class: SensingSourceClass::OpenDiscovery,
            possible_connection: None,
            sources: vec![SensingSource {
                url: "https://example.test/signal".to_owned(),
                detail: "Source support".to_owned(),
            }],
            actor: InputRoleSnapshot::ambient("gpt-test", "low"),
            observed_at: "2026-08-08T00:00:00.000Z".to_owned(),
            expires_at: "2026-08-09T00:00:00.000Z".to_owned(),
            fingerprint: "signal".to_owned(),
        }
    }

    #[tokio::test]
    async fn published_signal_is_visible_and_deduplicated_by_candidate() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-{nonce}.json"));
        let store = SignalStore::open(path.clone()).await.unwrap();

        let first = store
            .publish(&candidate("sense_1"), "credible".to_owned())
            .await
            .unwrap();
        let duplicate = store
            .publish(&candidate("sense_1"), "again".to_owned())
            .await
            .unwrap();

        assert_eq!(first.id, duplicate.id);
        assert_eq!(store.visible(10).await.unwrap().len(), 1);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn unreadable_local_signal_stream_is_reset() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-stale-{nonce}.json"));
        tokio::fs::write(&path, "not valid json").await.unwrap();

        let store = SignalStore::open(path.clone()).await.unwrap();

        assert!(store.visible(10).await.unwrap().is_empty());
        let _ = tokio::fs::remove_file(path).await;
    }
}
