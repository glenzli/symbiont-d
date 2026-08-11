use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

const CANDIDATE_TTL_HOURS: i64 = 24;
const MAX_PENDING_CANDIDATES: usize = 24;
const MAX_CANDIDATES_PER_INPUT_BATCH: usize = 3;
const MAX_REVIEW_BATCH_SIZE: usize = 12;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensingSource {
    pub url: String,
    pub detail: String,
}

/// Stable source identity for a model that can only contribute input.
///
/// The source id, provider and model are captured with each candidate. Name and
/// avatar are presentation fields and may be overlaid by the user's role
/// appearance settings when the timeline is rendered.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRoleSnapshot {
    pub id: String,
    pub name: String,
    pub model: String,
    pub effort: String,
    #[serde(alias = "avatar_seed")]
    pub avatar_seed: String,
    #[serde(default, alias = "provider_id")]
    pub provider_id: Option<String>,
    #[serde(default, alias = "channel_id")]
    pub channel_id: Option<String>,
}

impl InputRoleSnapshot {
    pub fn ambient(channel_id: &str, name: &str, model: &str, provider_id: &str) -> Self {
        let normalized = normalize(channel_id);
        Self {
            id: format!("ambient_{channel_id}"),
            name: name.to_owned(),
            model: model.to_owned(),
            effort: "input-only".to_owned(),
            avatar_seed: normalized,
            provider_id: Some(provider_id.to_owned()),
            channel_id: Some(channel_id.to_owned()),
        }
    }

    /// Presentation identity for a private inbox. The sender of each e-mail
    /// remains in the candidate provenance; the inbox itself is only the
    /// neutral transport role in the conversation timeline.
    pub fn mailbox(name: &str) -> Self {
        Self {
            id: "mail_inbox".to_owned(),
            name: name.to_owned(),
            model: "IMAP".to_owned(),
            effort: "input-only".to_owned(),
            avatar_seed: "mail-inbox".to_owned(),
            provider_id: Some("imap".to_owned()),
            channel_id: Some("research-inbox".to_owned()),
        }
    }

    /// Presentation identity for documents delivered through one private,
    /// user-configured Google Drive folder.
    pub fn drive(name: &str) -> Self {
        Self {
            id: "drive_digests".to_owned(),
            name: name.to_owned(),
            model: "Google Drive".to_owned(),
            effort: "input-only".to_owned(),
            avatar_seed: "drive-digests".to_owned(),
            provider_id: Some("google-drive".to_owned()),
            channel_id: Some("gemini-daily-digests".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SensingSourceClass {
    Research,
    ProductsAndTools,
    ProjectsAndEcosystems,
    InstitutionsAndPolicy,
    IndustryAndMarkets,
    CultureAndIdeas,
    #[default]
    OpenDiscovery,
}

impl SensingSourceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::ProductsAndTools => "products_and_tools",
            Self::ProjectsAndEcosystems => "projects_and_ecosystems",
            Self::InstitutionsAndPolicy => "institutions_and_policy",
            Self::IndustryAndMarkets => "industry_and_markets",
            Self::CultureAndIdeas => "culture_and_ideas",
            Self::OpenDiscovery => "open_discovery",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SensingPresentation {
    #[default]
    Original,
    Condensed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensingCandidateDraft {
    pub title: String,
    pub summary: String,
    pub proposed_input: String,
    /// The normalized text actually received from the input channel. Mail keeps
    /// the complete bounded section here; model channels fall back to their
    /// proposed input without pretending a fetched article body was supplied.
    #[serde(default)]
    pub received_text: Option<String>,
    #[serde(default)]
    pub event_at: Option<String>,
    #[serde(default)]
    pub source_class: SensingSourceClass,
    #[serde(default, alias = "relevance")]
    pub possible_connection: Option<String>,
    pub sources: Vec<SensingSource>,
}

/// Validate the bounded, transient handoff contract shared by every ambient
/// input provider.  A candidate is intentionally not trusted evidence, but it
/// must be compact and attributable before the stronger Codex review sees it.
pub fn validate_candidate_drafts(drafts: &[SensingCandidateDraft]) -> Result<()> {
    if drafts.len() > MAX_CANDIDATES_PER_INPUT_BATCH {
        anyhow::bail!(
            "ambient sensing accepts at most {MAX_CANDIDATES_PER_INPUT_BATCH} candidates"
        );
    }
    for candidate in drafts {
        for (label, value, limit) in [
            ("title", candidate.title.as_str(), 240),
            ("summary", candidate.summary.as_str(), 1_000),
            ("proposed_input", candidate.proposed_input.as_str(), 1_800),
        ] {
            if value.trim().is_empty() || value.chars().count() > limit {
                anyhow::bail!(
                    "ambient sensing candidate {label} must contain at most {limit} characters"
                );
            }
        }
        if candidate
            .received_text
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 24_000)
        {
            anyhow::bail!("ambient sensing candidate received_text exceeds 24000 characters");
        }
        if candidate
            .event_at
            .as_deref()
            .is_some_and(|value| value.chars().count() > 64)
        {
            anyhow::bail!("ambient sensing candidate event_at exceeds 64 characters");
        }
        if candidate
            .possible_connection
            .as_deref()
            .is_some_and(|value| value.chars().count() > 800)
        {
            anyhow::bail!("ambient sensing candidate possible_connection exceeds 800 characters");
        }
        if candidate.sources.is_empty() || candidate.sources.len() > 3 {
            anyhow::bail!("ambient sensing candidate requires one to three sources");
        }
        for source in &candidate.sources {
            if source.url.trim().is_empty()
                || source.detail.trim().is_empty()
                || source.url.chars().count() > 900
                || source.detail.chars().count() > 800
            {
                anyhow::bail!("ambient sensing source exceeds its compact contract");
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensingCandidate {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub proposed_input: String,
    pub received_text: String,
    #[serde(default)]
    pub event_at: Option<String>,
    #[serde(default)]
    pub source_class: SensingSourceClass,
    #[serde(default, alias = "relevance")]
    pub possible_connection: Option<String>,
    pub sources: Vec<SensingSource>,
    pub actor: InputRoleSnapshot,
    pub observed_at: String,
    pub expires_at: String,
    pub(crate) fingerprint: String,
}

#[derive(Default, Deserialize, Serialize)]
struct CandidatePool {
    candidates: Vec<SensingCandidate>,
    #[serde(default)]
    next_intake_channel: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensingIntakeBrief {
    pub id: &'static str,
    pub label: &'static str,
    pub brief: &'static str,
}

const INTAKE_CHANNELS: [SensingIntakeBrief; 6] = [
    SensingIntakeBrief {
        id: "research",
        label: "Research and methods",
        brief: "Scan recent research, measurements, evaluations, datasets, and methods across fields. Prefer changes that alter what can be known or tested, not another opinion about a familiar result.",
    },
    SensingIntakeBrief {
        id: "products_and_tools",
        label: "Products, evaluations, and use",
        brief: "Scan meaningful releases, independent evaluations, community experience, real adoption, useful applications, failures, deprecations, pricing, and access changes in tools and platforms. Distinguish official claims from what users can actually reproduce. Include non-AI tools when the change has unusual leverage.",
    },
    SensingIntakeBrief {
        id: "projects_and_ecosystems",
        label: "Projects and ecosystems",
        brief: "Scan open-source projects, standards, protocols, communities, and technical ecosystems for concrete new directions, surprising adoption, or important maintenance changes.",
    },
    SensingIntakeBrief {
        id: "institutions_and_policy",
        label: "Institutions and public affairs",
        brief: "Scan institutional, regulatory, educational, scientific, and public-affairs changes with durable practical or intellectual consequences. Avoid routine headline churn.",
    },
    SensingIntakeBrief {
        id: "industry_and_markets",
        label: "Industry and markets",
        brief: "Scan shifts in supply, business models, labor, infrastructure, and industry structure. Prefer evidence of a changed constraint or incentive over generic market commentary.",
    },
    SensingIntakeBrief {
        id: "culture_and_ideas",
        label: "Culture and ideas",
        brief: "Scan books, essays, creative practice, cultural debates, and emerging ideas for concrete developments or unusually useful frames. Do not require an immediate project connection.",
    },
];

pub struct SensingStore {
    path: PathBuf,
    pool: RwLock<CandidatePool>,
}

impl SensingStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let mut pool = match fs::read_to_string(&path).await {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(pool) => pool,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "discarding stale transient sensing candidate pool after schema change");
                    CandidatePool::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => CandidatePool::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read sensing candidate pool {}", path.display()));
            }
        };
        let changed = prune_expired(&mut pool, Utc::now());
        let store = Self {
            path,
            pool: RwLock::new(pool),
        };
        if changed {
            store.persist().await?;
        }
        Ok(store)
    }

    /// Adds a test batch to the short-lived intake queue.
    #[cfg(test)]
    pub async fn replace(
        &self,
        drafts: Vec<SensingCandidateDraft>,
        actor: InputRoleSnapshot,
    ) -> Result<usize> {
        self.enqueue_many(vec![(drafts, actor)]).await
    }

    /// Appends the results from every input role selected in one exploration
    /// cycle. Reviewed candidates are removed separately, so a multi-topic
    /// report can drain across several bounded review cycles without becoming
    /// durable memory or being lost when its mailbox cursor advances.
    pub async fn enqueue_many(
        &self,
        batches: Vec<(Vec<SensingCandidateDraft>, InputRoleSnapshot)>,
    ) -> Result<usize> {
        let now = Utc::now();
        let mut pool = self.pool.write().await;
        prune_expired(&mut pool, now);
        let mut appended = 0;
        for (drafts, actor) in batches {
            for draft in drafts {
                if pool.candidates.len() >= MAX_PENDING_CANDIDATES {
                    break;
                }
                let fingerprint = candidate_fingerprint(&draft);
                if pool
                    .candidates
                    .iter()
                    .any(|candidate| candidate.fingerprint == fingerprint)
                {
                    continue;
                }
                let sequence = pool.candidates.len();
                pool.candidates.push(build_candidate(
                    draft,
                    actor.clone(),
                    fingerprint,
                    now,
                    sequence,
                ));
                appended += 1;
            }
        }
        drop(pool);
        self.persist().await?;
        Ok(appended)
    }

    /// Atomically represents every candidate from one immutable transport
    /// document in the transient queue. Existing fingerprints count as
    /// represented; when the missing remainder cannot fit, nothing is added so
    /// the transport owner can leave its remote cursor uncommitted.
    pub async fn enqueue_complete_batch(
        &self,
        drafts: Vec<SensingCandidateDraft>,
        actor: InputRoleSnapshot,
    ) -> Result<bool> {
        let now = Utc::now();
        let mut pool = self.pool.write().await;
        prune_expired(&mut pool, now);
        let existing = pool
            .candidates
            .iter()
            .map(|candidate| candidate.fingerprint.clone())
            .collect::<HashSet<_>>();
        let mut missing = HashSet::new();
        for draft in &drafts {
            let fingerprint = candidate_fingerprint(draft);
            if !existing.contains(&fingerprint) {
                missing.insert(fingerprint);
            }
        }
        if pool.candidates.len() + missing.len() > MAX_PENDING_CANDIDATES {
            return Ok(false);
        }
        let mut appended = 0;
        let mut represented = existing;
        for draft in drafts {
            let fingerprint = candidate_fingerprint(&draft);
            if !represented.insert(fingerprint.clone()) {
                continue;
            }
            let sequence = pool.candidates.len();
            pool.candidates.push(build_candidate(
                draft,
                actor.clone(),
                fingerprint,
                now,
                sequence,
            ));
            appended += 1;
        }
        drop(pool);
        if appended > 0 {
            self.persist().await?;
        }
        Ok(true)
    }

    /// Removes candidates after the stronger review produced a terminal
    /// route. Missing decisions remain queued for a later bounded retry.
    pub async fn remove(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut pool = self.pool.write().await;
        pool.candidates
            .retain(|candidate| !ids.iter().any(|id| id == &candidate.id));
        drop(pool);
        self.persist().await
    }

    /// Applies one bounded review atomically. Terminal candidates leave the
    /// transient pool; candidates omitted by a malformed or partial model
    /// response move behind untouched work so they cannot block the queue
    /// head forever.
    pub async fn settle_review(
        &self,
        terminal_ids: &[String],
        deferred_ids: &[String],
    ) -> Result<()> {
        if terminal_ids.is_empty() && deferred_ids.is_empty() {
            return Ok(());
        }
        let terminal_ids = terminal_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        let deferred_ids = deferred_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        let mut pool = self.pool.write().await;
        let mut retained = Vec::with_capacity(pool.candidates.len());
        let mut deferred = Vec::new();
        for candidate in pool.candidates.drain(..) {
            if terminal_ids.contains(&candidate.id) {
                continue;
            }
            if deferred_ids.contains(&candidate.id) {
                deferred.push(candidate);
            } else {
                retained.push(candidate);
            }
        }
        retained.extend(deferred);
        pool.candidates = retained;
        drop(pool);
        self.persist().await
    }

    pub async fn next_intake_brief(&self) -> Result<SensingIntakeBrief> {
        let mut pool = self.pool.write().await;
        let index = pool.next_intake_channel % INTAKE_CHANNELS.len();
        let brief = INTAKE_CHANNELS[index];
        pool.next_intake_channel = (index + 1) % INTAKE_CHANNELS.len();
        drop(pool);
        self.persist().await?;
        Ok(brief)
    }

    pub async fn review_batch(&self, limit: usize) -> Result<Vec<SensingCandidate>> {
        let now = Utc::now();
        let mut pool = self.pool.write().await;
        let changed = prune_expired(&mut pool, now);
        let candidates = pool
            .candidates
            .iter()
            .take(limit.clamp(1, MAX_REVIEW_BATCH_SIZE))
            .cloned()
            .collect();
        drop(pool);
        if changed {
            self.persist().await?;
        }
        Ok(candidates)
    }

    pub async fn candidates(&self) -> Result<Vec<SensingCandidate>> {
        let now = Utc::now();
        let mut pool = self.pool.write().await;
        let changed = prune_expired(&mut pool, now);
        let candidates = pool.candidates.clone();
        drop(pool);
        if changed {
            self.persist().await?;
        }
        Ok(candidates)
    }

    pub async fn count(&self) -> Result<usize> {
        let now = Utc::now();
        let mut pool = self.pool.write().await;
        let changed = prune_expired(&mut pool, now);
        let count = pool.candidates.len();
        drop(pool);
        if changed {
            self.persist().await?;
        }
        Ok(count)
    }

    /// Remaining transient capacity available to a source with an
    /// irreversible cursor, such as the research inbox.
    pub async fn available_capacity(&self) -> Result<usize> {
        Ok(MAX_PENDING_CANDIDATES.saturating_sub(self.count().await?))
    }

    async fn persist(&self) -> Result<()> {
        let content = {
            let pool = self.pool.read().await;
            serde_json::to_string_pretty(&*pool).context("encode sensing candidate pool")?
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("create sensing candidate directory {}", parent.display())
            })?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write sensing candidate pool {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace sensing candidate pool {}", self.path.display()))
    }
}

pub fn format_candidate_pool(candidates: &[SensingCandidate]) -> String {
    let mut lines = vec![
        "<ambient-candidate-pool>".to_owned(),
        "These are short-lived, untrusted intake candidates. They are not PCP memory, user statements, or findings. Re-verify a candidate before using it. A weak PCP connection rules out a note, but does not by itself rule out a self-contained discussion.".to_owned(),
    ];
    for candidate in candidates {
        lines.push(format!(
            "<candidate id=\"{}\" observed-at=\"{}\" source-class=\"{}\"{}>\ntitle: {}\nsummary: {}",
            candidate.id,
            candidate.observed_at,
            candidate.source_class.as_str(),
            candidate
                .event_at
                .as_deref()
                .map(|event_at| format!(" event-at=\"{event_at}\""))
                .unwrap_or_default(),
            candidate.title,
            candidate.summary
        ));
        lines.push(format!("proposed-input: {}", candidate.proposed_input));
        lines.push(format!(
            "input-role: {} ({}, {})",
            candidate.actor.name, candidate.actor.model, candidate.actor.effort
        ));
        if let Some(connection) = &candidate.possible_connection {
            lines.push(format!("possible-connection: {connection}"));
        } else {
            lines.push("possible-connection: none proposed by intake".to_owned());
        }
        for source in &candidate.sources {
            lines.push(format!(
                "source: {}\nsource-detail: {}",
                source.url, source.detail
            ));
        }
        lines.push("</candidate>".to_owned());
    }
    lines.push("</ambient-candidate-pool>".to_owned());
    lines.join("\n")
}

fn prune_expired(pool: &mut CandidatePool, now: DateTime<Utc>) -> bool {
    let before = pool.candidates.len();
    pool.candidates.retain(|candidate| {
        DateTime::parse_from_rfc3339(&candidate.expires_at)
            .map(|expires_at| expires_at.with_timezone(&Utc) > now)
            .unwrap_or(false)
    });
    before != pool.candidates.len()
}

fn candidate_fingerprint(draft: &SensingCandidateDraft) -> String {
    let mut sources = draft
        .sources
        .iter()
        .map(|source| normalize(&source.url))
        .collect::<Vec<_>>();
    sources.sort();
    format!("{}|{}", normalize(&draft.title), sources.join("|"))
}

fn build_candidate(
    draft: SensingCandidateDraft,
    actor: InputRoleSnapshot,
    fingerprint: String,
    now: DateTime<Utc>,
    sequence: usize,
) -> SensingCandidate {
    SensingCandidate {
        id: format!("sense_{}_{}", now.timestamp_millis(), sequence),
        title: draft.title.trim().to_owned(),
        summary: draft.summary.trim().to_owned(),
        proposed_input: draft.proposed_input.trim().to_owned(),
        received_text: draft
            .received_text
            .as_deref()
            .unwrap_or(&draft.proposed_input)
            .trim()
            .to_owned(),
        event_at: draft
            .event_at
            .map(|event_at| event_at.trim().to_owned())
            .filter(|event_at| !event_at.is_empty()),
        source_class: draft.source_class,
        possible_connection: draft
            .possible_connection
            .map(|connection| connection.trim().to_owned())
            .filter(|connection| !connection.is_empty()),
        sources: draft.sources,
        actor,
        observed_at: timestamp(now),
        expires_at: timestamp(now + Duration::hours(CANDIDATE_TTL_HOURS)),
        fingerprint,
    }
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

    use super::{
        InputRoleSnapshot, MAX_CANDIDATES_PER_INPUT_BATCH, MAX_PENDING_CANDIDATES,
        MAX_REVIEW_BATCH_SIZE, SensingCandidateDraft, SensingSource, SensingSourceClass,
        SensingStore, format_candidate_pool, validate_candidate_drafts,
    };

    fn candidate(title: &str, url: &str) -> SensingCandidateDraft {
        SensingCandidateDraft {
            title: title.to_owned(),
            summary: "A short factual change.".to_owned(),
            proposed_input: "A short model input.".to_owned(),
            received_text: None,
            event_at: None,
            source_class: SensingSourceClass::Research,
            possible_connection: None,
            sources: vec![SensingSource {
                url: url.to_owned(),
                detail: "Primary source.".to_owned(),
            }],
        }
    }

    #[test]
    fn intake_batch_limit_remains_source_local() {
        let drafts = (0..=MAX_CANDIDATES_PER_INPUT_BATCH)
            .map(|index| {
                candidate(
                    &format!("Candidate {index}"),
                    &format!("https://example.test/{index}"),
                )
            })
            .collect::<Vec<_>>();

        assert!(validate_candidate_drafts(&drafts[..MAX_CANDIDATES_PER_INPUT_BATCH]).is_ok());
        assert!(validate_candidate_drafts(&drafts).is_err());
    }

    #[tokio::test]
    async fn review_can_drain_multiple_input_batches_at_once() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-review-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");
        let drafts = (0..MAX_REVIEW_BATCH_SIZE)
            .map(|index| {
                candidate(
                    &format!("Candidate {index}"),
                    &format!("https://example.test/review/{index}"),
                )
            })
            .collect();
        store.replace(drafts, actor).await.unwrap();

        assert_eq!(
            store
                .review_batch(MAX_REVIEW_BATCH_SIZE)
                .await
                .unwrap()
                .len(),
            MAX_REVIEW_BATCH_SIZE
        );

        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn unreviewed_candidates_remain_queued_across_intake_passes() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");
        assert_eq!(
            store
                .replace(
                    vec![candidate("Old signal", "https://example.test/old")],
                    actor.clone()
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .replace(
                    vec![candidate("New signal", "https://example.test/news")],
                    actor
                )
                .await
                .unwrap(),
            1
        );
        let batch = store.review_batch(3).await.unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].title, "Old signal");
        assert_eq!(batch[1].title, "New signal");
        store.remove(&[batch[0].id.clone()]).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);
        assert_eq!(store.review_batch(3).await.unwrap()[0].title, "New signal");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn partial_review_moves_omitted_candidates_behind_fresh_work() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-deferred-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");
        store
            .replace(
                vec![
                    candidate("First", "https://example.test/first"),
                    candidate("Omitted", "https://example.test/omitted"),
                    candidate("Third", "https://example.test/third"),
                ],
                actor,
            )
            .await
            .unwrap();
        let batch = store.review_batch(3).await.unwrap();

        store
            .settle_review(&[batch[0].id.clone()], &[batch[1].id.clone()])
            .await
            .unwrap();

        let remaining = store.candidates().await.unwrap();
        assert_eq!(remaining[0].title, "Third");
        assert_eq!(remaining[1].title, "Omitted");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn available_capacity_tracks_the_bounded_transient_pool() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-capacity-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");

        assert_eq!(
            store.available_capacity().await.unwrap(),
            MAX_PENDING_CANDIDATES
        );
        store
            .replace(vec![candidate("One", "https://example.test/one")], actor)
            .await
            .unwrap();
        assert_eq!(
            store.available_capacity().await.unwrap(),
            MAX_PENDING_CANDIDATES - 1
        );

        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn complete_document_batch_is_all_or_nothing_when_capacity_is_tight() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-atomic-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");
        let existing = (0..MAX_PENDING_CANDIDATES - 1)
            .map(|index| {
                candidate(
                    &format!("Existing {index}"),
                    &format!("https://example.test/existing/{index}"),
                )
            })
            .collect();
        store.replace(existing, actor.clone()).await.unwrap();

        let accepted = store
            .enqueue_complete_batch(
                vec![
                    candidate("Document first", "https://example.test/document/first"),
                    candidate("Document second", "https://example.test/document/second"),
                ],
                actor,
            )
            .await
            .unwrap();

        assert!(!accepted);
        assert_eq!(store.count().await.unwrap(), MAX_PENDING_CANDIDATES - 1);
        assert!(
            store
                .candidates()
                .await
                .unwrap()
                .iter()
                .all(|entry| !entry.title.starts_with("Document "))
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn intake_channels_rotate_and_persist_independently_of_candidates() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-rotation-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        assert_eq!(store.next_intake_brief().await.unwrap().id, "research");
        assert_eq!(
            store.next_intake_brief().await.unwrap().id,
            "products_and_tools"
        );
        drop(store);

        let reopened = SensingStore::open(path.clone()).await.unwrap();
        assert_eq!(
            reopened.next_intake_brief().await.unwrap().id,
            "projects_and_ecosystems"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn candidates_do_not_require_a_preexisting_user_connection() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-open-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");
        store
            .replace(
                vec![candidate(
                    "Unfamiliar but credible signal",
                    "https://example.test/open",
                )],
                actor,
            )
            .await
            .unwrap();
        let pool = format_candidate_pool(&store.review_batch(1).await.unwrap());
        assert!(pool.contains("possible-connection: none proposed by intake"));
        assert!(pool.contains("source-class=\"research\""));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn candidate_pool_preserves_the_external_event_date() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-event-{nonce}.json"));
        let store = SensingStore::open(path.clone()).await.unwrap();
        let actor = InputRoleSnapshot::ambient("test", "Test observer", "test", "test-provider");
        let mut draft = candidate("Still-active recent event", "https://example.test/event");
        draft.event_at = Some("2026-07-24".to_owned());
        store.replace(vec![draft], actor).await.unwrap();

        let pool = format_candidate_pool(&store.review_batch(1).await.unwrap());
        assert!(pool.contains("event-at=\"2026-07-24\""));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_relevance_field_migrates_to_optional_connection() {
        let draft: SensingCandidateDraft = serde_json::from_value(serde_json::json!({
            "title": "Legacy signal",
            "summary": "Previously stored shape",
            "proposed_input": "A model input",
            "relevance": "A tentative old connection",
            "sources": [{"url": "https://example.test/legacy", "detail": "Primary"}]
        }))
        .unwrap();
        assert_eq!(draft.source_class, SensingSourceClass::OpenDiscovery);
        assert_eq!(
            draft.possible_connection.as_deref(),
            Some("A tentative old connection")
        );
    }

    #[tokio::test]
    async fn stale_candidate_pool_is_discarded_after_a_schema_change() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-sensing-legacy-{nonce}.json"));
        tokio::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "candidates": [{
                    "id": "sense_legacy",
                    "title": "Legacy stored signal",
                    "summary": "Stored before source classes were introduced.",
                    "relevance": "An old tentative connection",
                    "sources": [{"url": "https://example.test/legacy", "detail": "Primary"}],
                    "observed_at": "2026-08-01T00:00:00.000Z",
                    "expires_at": "2999-01-01T00:00:00.000Z",
                    "fingerprint": "legacy"
                }]
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let store = SensingStore::open(path.clone()).await.unwrap();
        assert!(store.review_batch(1).await.unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
