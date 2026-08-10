use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::external_markdown::normalize_external_markdown;
use crate::sensing::{
    InputRoleSnapshot, SensingCandidate, SensingPresentation, SensingSource, SensingSourceClass,
};

const RETENTION_DAYS: i64 = 30;
const MAX_EVENT_AGE_DAYS: i64 = 45;
const MAX_RETAINED_SIGNALS: usize = 100;

/// A visible but non-durable input from an auxiliary model role.
///
/// Signals deliberately live outside PCP. They become durable source material only
/// when the user explicitly replies to one.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalEvent {
    pub id: String,
    #[serde(alias = "candidate_id")]
    pub candidate_id: String,
    /// A stable identity derived from the underlying event and sources. Candidate
    /// IDs are regenerated for every sensing pass, so they cannot prevent an old
    /// event from being reintroduced as a new input.
    #[serde(default)]
    pub fingerprint: String,
    pub actor: InputRoleSnapshot,
    pub content: String,
    #[serde(default)]
    #[serde(alias = "received_text")]
    pub received_text: String,
    #[serde(default)]
    pub presentation: SensingPresentation,
    #[serde(default)]
    #[serde(alias = "qualification_note")]
    pub qualification_note: Option<String>,
    pub title: String,
    pub summary: String,
    pub sources: Vec<SensingSource>,
    #[serde(alias = "source_class")]
    pub source_class: SensingSourceClass,
    #[serde(default, alias = "event_at")]
    pub event_at: Option<String>,
    #[serde(alias = "observed_at")]
    pub observed_at: String,
    #[serde(alias = "review_reason")]
    pub review_reason: String,
    #[serde(default, alias = "promoted_revision_id")]
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

#[derive(Clone, Debug)]
pub enum SignalPublishOutcome {
    Published,
    Existing,
    RejectedStale,
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
        let changed = normalize_and_prune(&mut document, Utc::now());
        let store = Self {
            path,
            document: RwLock::new(document),
        };
        if changed {
            store.persist().await?;
        }
        Ok(store)
    }

    #[cfg(test)]
    pub async fn publish_with_content(
        &self,
        candidate: &SensingCandidate,
        content: String,
        review_reason: String,
    ) -> Result<SignalPublishOutcome> {
        self.publish_with_presentation(
            candidate,
            content,
            SensingPresentation::Original,
            None,
            review_reason,
        )
        .await
    }

    pub async fn publish_with_presentation(
        &self,
        candidate: &SensingCandidate,
        content: String,
        presentation: SensingPresentation,
        qualification_note: Option<String>,
        review_reason: String,
    ) -> Result<SignalPublishOutcome> {
        let now = Utc::now();
        if event_is_too_old(candidate.event_at.as_deref(), now) {
            tracing::info!(
                title = %candidate.title,
                event_at = ?candidate.event_at,
                "skipping stale external input signal"
            );
            return Ok(SignalPublishOutcome::RejectedStale);
        }
        let mut document = self.document.write().await;
        if document
            .signals
            .iter()
            .find(|signal| {
                signal.candidate_id == candidate.id || signal.fingerprint == candidate.fingerprint
            })
            .is_some()
        {
            return Ok(SignalPublishOutcome::Existing);
        }
        let sequence = document.signals.len();
        let event = SignalEvent {
            id: format!("signal_{}_{}", now.timestamp_millis(), sequence),
            candidate_id: candidate.id.clone(),
            fingerprint: candidate.fingerprint.clone(),
            actor: candidate.actor.clone(),
            content: content.trim().to_owned(),
            received_text: candidate.received_text.clone(),
            presentation,
            qualification_note: qualification_note
                .map(|note| note.trim().to_owned())
                .filter(|note| !note.is_empty()),
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
        normalize_and_prune(&mut document, now);
        drop(document);
        self.persist().await?;
        Ok(SignalPublishOutcome::Published)
    }

    pub async fn visible(&self, limit: usize) -> Result<Vec<SignalEvent>> {
        let now = Utc::now();
        let mut document = self.document.write().await;
        let changed = normalize_and_prune(&mut document, now);
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

    /// Removes a temporary signal from the visible conversation without
    /// turning that action into preference feedback. Keep the hidden record so
    /// the same external event is not published again by a later sensing pass.
    pub async fn dismiss(&self, id: &str) -> Result<bool> {
        let mut document = self.document.write().await;
        let dismissed = document
            .signals
            .iter_mut()
            .find(|signal| signal.id == id)
            .map(|signal| {
                signal.hidden = true;
            })
            .is_some();
        drop(document);
        if dismissed {
            self.persist().await?;
        }
        Ok(dismissed)
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
                // A signal is only a temporary input-role message. Once the user
                // chooses to discuss it, its immutable source page and the
                // ensuing conversation carry the context instead.
                signal.hidden = true;
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

fn normalize_and_prune(document: &mut SignalDocument, now: DateTime<Utc>) -> bool {
    let mut changed = false;
    for signal in &mut document.signals {
        if signal.fingerprint.trim().is_empty() {
            signal.fingerprint = signal_fingerprint(&signal.title, &signal.sources);
            changed = true;
        }
        if signal.received_text.trim().is_empty() {
            signal.received_text = if signal.summary.trim().is_empty() {
                signal.content.clone()
            } else {
                signal.summary.clone()
            };
            if signal.received_text.trim() != signal.content.trim() {
                signal.presentation = SensingPresentation::Condensed;
            }
            changed = true;
        }
        let content = normalize_external_markdown(&signal.content);
        if content != signal.content {
            signal.content = content;
            changed = true;
        }
        let received_text = normalize_external_markdown(&signal.received_text);
        if received_text != signal.received_text {
            signal.received_text = received_text;
            changed = true;
        }
        let summary = normalize_external_markdown(&signal.summary);
        if summary != signal.summary {
            signal.summary = summary;
            changed = true;
        }
    }
    let before = document.signals.len();
    let oldest = now - Duration::days(RETENTION_DAYS);
    document.signals.retain(|signal| {
        DateTime::parse_from_rfc3339(&signal.observed_at)
            .map(|observed_at| observed_at.with_timezone(&Utc) >= oldest)
            .unwrap_or(false)
            && !event_is_too_old(signal.event_at.as_deref(), now)
    });
    if document.signals.len() > MAX_RETAINED_SIGNALS {
        let drop_count = document.signals.len() - MAX_RETAINED_SIGNALS;
        document.signals.drain(0..drop_count);
    }
    changed || before != document.signals.len()
}

fn event_is_too_old(event_at: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(event_at) = event_at else {
        return false;
    };
    let date = DateTime::parse_from_rfc3339(event_at)
        .map(|value| value.date_naive())
        .or_else(|_| NaiveDate::parse_from_str(event_at.trim(), "%Y-%m-%d"));
    date.map(|date| date < (now - Duration::days(MAX_EVENT_AGE_DAYS)).date_naive())
        .unwrap_or(false)
}

fn signal_fingerprint(title: &str, sources: &[SensingSource]) -> String {
    let mut urls = sources
        .iter()
        .map(|source| normalize(&source.url))
        .collect::<Vec<_>>();
    urls.sort();
    format!("{}|{}", normalize(title), urls.join("|"))
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{SignalPublishOutcome, SignalStore};
    use crate::sensing::{
        InputRoleSnapshot, SensingCandidate, SensingPresentation, SensingSource, SensingSourceClass,
    };

    fn candidate(id: &str) -> SensingCandidate {
        SensingCandidate {
            id: id.to_owned(),
            fingerprint: format!("fingerprint-{id}"),
            title: "A signal".to_owned(),
            summary: "A compact summary".to_owned(),
            proposed_input: "A model input.".to_owned(),
            received_text: "A model input.".to_owned(),
            event_at: None,
            source_class: SensingSourceClass::OpenDiscovery,
            possible_connection: None,
            sources: vec![SensingSource {
                url: "https://example.test/signal".to_owned(),
                detail: "Source support".to_owned(),
            }],
            actor: InputRoleSnapshot::ambient("test", "Test observer", "gpt-test", "test-provider"),
            observed_at: "2026-08-08T00:00:00.000Z".to_owned(),
            expires_at: "2026-08-09T00:00:00.000Z".to_owned(),
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

        let first_outcome = store
            .publish_with_content(
                &candidate("sense_1"),
                "A model input.".to_owned(),
                "credible".to_owned(),
            )
            .await
            .unwrap();
        let duplicate_outcome = store
            .publish_with_content(
                &candidate("sense_1"),
                "A model input.".to_owned(),
                "again".to_owned(),
            )
            .await
            .unwrap();

        assert!(matches!(first_outcome, SignalPublishOutcome::Published));
        assert!(matches!(duplicate_outcome, SignalPublishOutcome::Existing));
        let visible = store.visible(10).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].candidate_id, "sense_1");
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn condensed_presentation_preserves_received_text_and_separates_qualification() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-safe-{nonce}.json"));
        let store = SignalStore::open(path.clone()).await.unwrap();
        let candidate = candidate("sense_safe");

        let published_outcome = store
            .publish_with_presentation(
                &candidate,
                "A shorter attributed display.".to_owned(),
                SensingPresentation::Condensed,
                Some("The linked claim was not independently checked here.".to_owned()),
                "Interesting input with qualified certainty.".to_owned(),
            )
            .await
            .unwrap();
        assert!(matches!(published_outcome, SignalPublishOutcome::Published));
        let visible = store.visible(10).await.unwrap();
        let published = &visible[0];

        assert_eq!(published.actor.id, candidate.actor.id);
        assert_eq!(published.content, "A shorter attributed display.");
        assert_eq!(published.received_text, "A model input.");
        assert_eq!(published.presentation, SensingPresentation::Condensed);
        assert!(published.qualification_note.is_some());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn visible_signal_folds_naked_tracking_links_into_markdown() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-links-{nonce}.json"));
        let store = SignalStore::open(path.clone()).await.unwrap();
        let mut candidate = candidate("sense_links");
        let raw = "查看论文详情 →\nhttps://www.google.com/url?q=https%3A%2F%2Farxiv.org%2Fabs%2F2608.00086&source=gmail";
        candidate.received_text = raw.to_owned();

        store
            .publish_with_content(&candidate, raw.to_owned(), "credible".to_owned())
            .await
            .unwrap();
        let signal = store.visible(10).await.unwrap().remove(0);

        assert_eq!(
            signal.content,
            "[查看论文详情](<https://arxiv.org/abs/2608.00086>)"
        );
        assert_eq!(signal.received_text, signal.content);
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

    #[tokio::test]
    async fn stale_events_are_not_published_or_replayed() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-old-{nonce}.json"));
        let store = SignalStore::open(path.clone()).await.unwrap();
        let mut old = candidate("sense_old");
        old.event_at = Some("2025-01-01".to_owned());

        assert!(matches!(
            store
                .publish_with_content(&old, old.proposed_input.clone(), "credible".to_owned())
                .await
                .unwrap(),
            SignalPublishOutcome::RejectedStale
        ));
        assert!(store.visible(10).await.unwrap().is_empty());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn stable_fingerprint_deduplicates_a_new_sensing_pass() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-fingerprint-{nonce}.json"));
        let store = SignalStore::open(path.clone()).await.unwrap();
        let first_outcome = store
            .publish_with_content(
                &candidate("sense_first"),
                "A model input.".to_owned(),
                "credible".to_owned(),
            )
            .await
            .unwrap();
        assert!(matches!(first_outcome, SignalPublishOutcome::Published));
        let first = store.visible(10).await.unwrap().remove(0);
        let mut repeated = candidate("sense_second");
        repeated.fingerprint = first.fingerprint.clone();
        let again_outcome = store
            .publish_with_content(
                &repeated,
                repeated.proposed_input.clone(),
                "credible again".to_owned(),
            )
            .await
            .unwrap();
        assert!(matches!(again_outcome, SignalPublishOutcome::Existing));
        let again = store.visible(10).await.unwrap().remove(0);
        assert_eq!(first.id, again.id);
        assert_eq!(store.visible(10).await.unwrap().len(), 1);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn dismissed_signal_stays_hidden_without_becoming_republishable() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-signals-dismissed-{nonce}.json"));
        let store = SignalStore::open(path.clone()).await.unwrap();
        let original = candidate("sense_dismissed");

        assert!(matches!(
            store
                .publish_with_content(
                    &original,
                    original.proposed_input.clone(),
                    "credible".to_owned(),
                )
                .await
                .unwrap(),
            SignalPublishOutcome::Published
        ));
        let signal = store.visible(10).await.unwrap().remove(0);
        assert!(store.dismiss(&signal.id).await.unwrap());
        assert!(store.visible(10).await.unwrap().is_empty());
        assert!(store.get(&signal.id).await.unwrap().unwrap().hidden);

        let mut repeated = candidate("sense_dismissed_again");
        repeated.fingerprint = original.fingerprint;
        assert!(matches!(
            store
                .publish_with_content(
                    &repeated,
                    repeated.proposed_input.clone(),
                    "credible again".to_owned(),
                )
                .await
                .unwrap(),
            SignalPublishOutcome::Existing
        ));
        assert!(store.visible(10).await.unwrap().is_empty());
        let _ = tokio::fs::remove_file(path).await;
    }
}
