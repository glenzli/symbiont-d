use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Mutex, sync::mpsc};

const DEFAULT_SETTLE_SECONDS: i64 = 12;
const MAX_ACTIVE_INTENTS: usize = 32;
const RETAINED_INTENTS: usize = 80;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationIntentOrigin {
    Interactive,
    Reflection,
}

impl ExplorationIntentOrigin {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "interactive" => Some(Self::Interactive),
            "reflection" => Some(Self::Reflection),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Reflection => "reflection",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationIntentStatus {
    Queued,
    Exploring,
    Silent,
    Messaged,
    Superseded,
    Failed,
}

impl ExplorationIntentStatus {
    fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Exploring)
    }

    pub fn is_terminal(self) -> bool {
        !self.is_active()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationIntent {
    pub id: String,
    pub question: String,
    pub why_now: String,
    pub source_revision_ids: Vec<String>,
    pub origin: ExplorationIntentOrigin,
    pub status: ExplorationIntentStatus,
    pub requested_at: String,
    pub not_before: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub trace_id: Option<String>,
    pub result_revision_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewExplorationIntent {
    pub question: String,
    pub why_now: String,
    pub source_revision_ids: Vec<String>,
    pub origin: ExplorationIntentOrigin,
    pub not_before: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationIntentReceipt {
    pub id: String,
    pub deduplicated: bool,
    pub intent: ExplorationIntent,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExplorationIntentDocument {
    intents: Vec<ExplorationIntent>,
}

pub struct ExplorationIntentReceiver {
    receiver: mpsc::Receiver<String>,
}

impl ExplorationIntentReceiver {
    pub(super) async fn recv(&mut self) -> Option<String> {
        self.receiver.recv().await
    }
}

pub struct ExplorationIntentQueue {
    path: PathBuf,
    intents: Mutex<Vec<ExplorationIntent>>,
    sender: mpsc::Sender<String>,
}

impl ExplorationIntentQueue {
    pub async fn open(path: PathBuf) -> Result<(Self, ExplorationIntentReceiver)> {
        let mut intents = match fs::read_to_string(&path).await {
            Ok(content) => {
                serde_json::from_str::<ExplorationIntentDocument>(&content)
                    .with_context(|| {
                        format!("parse exploration intent journal {}", path.display())
                    })?
                    .intents
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read exploration intent journal {}", path.display())
                });
            }
        };
        for intent in &mut intents {
            if intent.status == ExplorationIntentStatus::Exploring {
                intent.status = ExplorationIntentStatus::Queued;
                intent.started_at = None;
                intent.error = None;
            }
        }
        trim_intents(&mut intents);
        let queued_ids = intents
            .iter()
            .filter(|intent| intent.status == ExplorationIntentStatus::Queued)
            .map(|intent| intent.id.clone())
            .collect::<Vec<_>>();
        let (sender, receiver) = mpsc::channel(64);
        let queue = Self {
            path,
            intents: Mutex::new(intents),
            sender,
        };
        {
            let intents = queue.intents.lock().await;
            queue.persist_locked(&intents).await?;
        }
        for id in queued_ids {
            queue
                .sender
                .try_send(id)
                .context("restore queued exploration intent")?;
        }
        Ok((queue, ExplorationIntentReceiver { receiver }))
    }

    pub async fn enqueue(&self, input: NewExplorationIntent) -> Result<ExplorationIntentReceipt> {
        validate_input(&input)?;
        let now = Utc::now();
        let not_before = normalized_not_before(input.not_before.as_deref(), now)?;
        let key = semantic_key(&input.question);
        let mut intents = self.intents.lock().await;
        if let Some(intent) = intents
            .iter_mut()
            .find(|intent| intent.status.is_active() && semantic_key(&intent.question) == key)
        {
            intent.why_now = input.why_now.trim().to_owned();
            intent.source_revision_ids.extend(input.source_revision_ids);
            intent.source_revision_ids.sort();
            intent.source_revision_ids.dedup();
            intent.source_revision_ids.truncate(50);
            let receipt = ExplorationIntentReceipt {
                id: intent.id.clone(),
                deduplicated: true,
                intent: intent.clone(),
            };
            self.persist_locked(&intents).await?;
            return Ok(receipt);
        }
        if intents
            .iter()
            .filter(|intent| intent.status.is_active())
            .count()
            >= MAX_ACTIVE_INTENTS
        {
            anyhow::bail!("too many exploration intents are already active");
        }
        let id = format!("intent_{:x}_{:x}", now.timestamp_micros(), intents.len());
        let intent = ExplorationIntent {
            id: id.clone(),
            question: input.question.trim().to_owned(),
            why_now: input.why_now.trim().to_owned(),
            source_revision_ids: normalized_sources(input.source_revision_ids),
            origin: input.origin,
            status: ExplorationIntentStatus::Queued,
            requested_at: timestamp(now),
            not_before,
            started_at: None,
            completed_at: None,
            trace_id: None,
            result_revision_id: None,
            error: None,
        };
        intents.push(intent.clone());
        trim_intents(&mut intents);
        self.persist_locked(&intents).await?;
        drop(intents);
        if self.sender.send(id.clone()).await.is_err() {
            self.complete(
                &id,
                ExplorationIntentStatus::Failed,
                None,
                None,
                Some("exploration scheduler is unavailable".to_owned()),
            )
            .await?;
            anyhow::bail!("exploration scheduler is unavailable");
        }
        Ok(ExplorationIntentReceipt {
            id,
            deduplicated: false,
            intent,
        })
    }

    pub async fn get(&self, id: &str) -> Option<ExplorationIntent> {
        self.intents
            .lock()
            .await
            .iter()
            .find(|intent| intent.id == id)
            .cloned()
    }

    pub async fn claim(&self, id: &str) -> Result<Option<ExplorationIntent>> {
        self.claim_at(id, Utc::now()).await
    }

    async fn claim_at(&self, id: &str, now: DateTime<Utc>) -> Result<Option<ExplorationIntent>> {
        let mut intents = self.intents.lock().await;
        let Some(intent) = intents
            .iter_mut()
            .find(|intent| intent.id == id && intent.status == ExplorationIntentStatus::Queued)
        else {
            return Ok(None);
        };
        let due = DateTime::parse_from_rfc3339(&intent.not_before)
            .context("parse exploration intent not-before time")?
            .with_timezone(&Utc);
        if due > now {
            return Ok(None);
        }
        intent.status = ExplorationIntentStatus::Exploring;
        intent.started_at = Some(timestamp(now));
        intent.error = None;
        let claimed = intent.clone();
        self.persist_locked(&intents).await?;
        Ok(Some(claimed))
    }

    pub async fn complete(
        &self,
        id: &str,
        status: ExplorationIntentStatus,
        trace_id: Option<String>,
        result_revision_id: Option<String>,
        error: Option<String>,
    ) -> Result<Option<ExplorationIntent>> {
        if !status.is_terminal() {
            anyhow::bail!("exploration intent completion requires a terminal status");
        }
        let mut intents = self.intents.lock().await;
        let Some(intent) = intents.iter_mut().find(|intent| intent.id == id) else {
            return Ok(None);
        };
        intent.status = status;
        intent.completed_at = Some(timestamp(Utc::now()));
        intent.trace_id = trace_id;
        intent.result_revision_id = result_revision_id;
        intent.error = error;
        let completed = intent.clone();
        trim_intents(&mut intents);
        self.persist_locked(&intents).await?;
        Ok(Some(completed))
    }

    pub async fn supersede(&self, ids: &[String], reason: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = timestamp(Utc::now());
        let mut intents = self.intents.lock().await;
        for intent in intents.iter_mut().filter(|intent| {
            ids.contains(&intent.id) && intent.status == ExplorationIntentStatus::Queued
        }) {
            intent.status = ExplorationIntentStatus::Superseded;
            intent.completed_at = Some(now.clone());
            intent.error = Some(reason.to_owned());
        }
        trim_intents(&mut intents);
        self.persist_locked(&intents).await
    }

    pub async fn recent(&self, limit: usize) -> Vec<ExplorationIntent> {
        self.intents
            .lock()
            .await
            .iter()
            .rev()
            .take(limit.clamp(1, 50))
            .cloned()
            .collect()
    }

    async fn persist_locked(&self, intents: &[ExplorationIntent]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("create exploration intent directory {}", parent.display())
            })?;
        }
        let content = serde_json::to_vec_pretty(&ExplorationIntentDocument {
            intents: intents.to_vec(),
        })?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write exploration intents {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace exploration intents {}", self.path.display()))
    }
}

fn validate_input(input: &NewExplorationIntent) -> Result<()> {
    let question_chars = input.question.trim().chars().count();
    if !(1..=1_200).contains(&question_chars) {
        anyhow::bail!("exploration question must contain 1 to 1200 characters");
    }
    let why_chars = input.why_now.trim().chars().count();
    if !(1..=1_600).contains(&why_chars) {
        anyhow::bail!("exploration rationale must contain 1 to 1600 characters");
    }
    if input.source_revision_ids.is_empty() || input.source_revision_ids.len() > 50 {
        anyhow::bail!("exploration intent requires 1 to 50 source Revisions");
    }
    Ok(())
}

fn normalized_not_before(value: Option<&str>, now: DateTime<Utc>) -> Result<String> {
    let settled = now + Duration::seconds(DEFAULT_SETTLE_SECONDS);
    let requested = value
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .context("exploration intent not_before must be RFC 3339")?
        .map(|value| value.with_timezone(&Utc));
    if requested.is_some_and(|value| value > now + Duration::days(30)) {
        anyhow::bail!("exploration intent not_before cannot exceed 30 days");
    }
    Ok(timestamp(requested.unwrap_or(settled).max(settled)))
}

fn normalized_sources(mut sources: Vec<String>) -> Vec<String> {
    sources.sort();
    sources.dedup();
    sources.truncate(50);
    sources
}

fn semantic_key(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn trim_intents(intents: &mut Vec<ExplorationIntent>) {
    let mut terminal_to_remove = intents.len().saturating_sub(RETAINED_INTENTS);
    intents.retain(|intent| {
        if terminal_to_remove > 0 && intent.status.is_terminal() {
            terminal_to_remove -= 1;
            false
        } else {
            true
        }
    });
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[tokio::test]
    async fn deduplicates_active_questions_and_preserves_new_sources() {
        let path = test_path("dedupe");
        let (queue, mut receiver) = ExplorationIntentQueue::open(path.clone()).await.unwrap();
        let first = queue
            .enqueue(input("Could this matter?", "rev_a"))
            .await
            .unwrap();
        let second = queue
            .enqueue(input("  could   this MATTER? ", "rev_b"))
            .await
            .unwrap();

        assert!(!first.deduplicated);
        assert!(second.deduplicated);
        assert_eq!(first.id, second.id);
        assert_eq!(second.intent.source_revision_ids, vec!["rev_a", "rev_b"]);
        assert_eq!(receiver.recv().await.as_deref(), Some(first.id.as_str()));

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn recovers_an_interrupted_intent_and_records_its_terminal_result() {
        let path = test_path("recover");
        let (queue, mut receiver) = ExplorationIntentQueue::open(path.clone()).await.unwrap();
        let receipt = queue
            .enqueue(input("Investigate this", "rev_a"))
            .await
            .unwrap();
        assert_eq!(receiver.recv().await.as_deref(), Some(receipt.id.as_str()));
        let future = Utc::now() + Duration::minutes(1);
        let claimed = queue.claim_at(&receipt.id, future).await.unwrap().unwrap();
        assert_eq!(claimed.status, ExplorationIntentStatus::Exploring);
        drop(queue);
        drop(receiver);

        let (reopened, mut reopened_receiver) =
            ExplorationIntentQueue::open(path.clone()).await.unwrap();
        assert_eq!(
            reopened_receiver.recv().await.as_deref(),
            Some(receipt.id.as_str())
        );
        let recovered = reopened.get(&receipt.id).await.unwrap();
        assert_eq!(recovered.status, ExplorationIntentStatus::Queued);
        let claimed = reopened
            .claim_at(&receipt.id, future)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.status, ExplorationIntentStatus::Exploring);
        let completed = reopened
            .complete(
                &receipt.id,
                ExplorationIntentStatus::Silent,
                Some("trace-1".to_owned()),
                None,
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, ExplorationIntentStatus::Silent);
        assert_eq!(completed.trace_id.as_deref(), Some("trace-1"));

        let _ = fs::remove_file(path).await;
    }

    fn input(question: &str, source: &str) -> NewExplorationIntent {
        NewExplorationIntent {
            question: question.to_owned(),
            why_now: "The current conversation exposed a concrete uncertainty.".to_owned(),
            source_revision_ids: vec![source.to_owned()],
            origin: ExplorationIntentOrigin::Interactive,
            not_before: None,
        }
    }

    fn test_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("symbiont-exploration-intents-{label}-{nonce}.json"))
    }
}
