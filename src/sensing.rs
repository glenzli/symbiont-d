use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

const CANDIDATE_TTL_HOURS: i64 = 24;
pub const REVIEW_BATCH_SIZE: usize = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensingSource {
    pub url: String,
    pub detail: String,
}

/// Immutable presentation identity for a model that can only contribute input.
///
/// It is captured with each candidate so later compute-setting changes never
/// relabel a signal that has already appeared in the conversation timeline.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InputRoleSnapshot {
    pub id: String,
    pub name: String,
    pub model: String,
    pub effort: String,
    pub avatar_seed: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensingCandidateDraft {
    pub title: String,
    pub summary: String,
    pub proposed_input: String,
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
    if drafts.len() > REVIEW_BATCH_SIZE {
        anyhow::bail!("ambient sensing accepts at most {REVIEW_BATCH_SIZE} candidates");
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

    /// Replaces the short-lived intake pool after a new sensing pass.
    ///
    /// Candidates are deliberately not durable memory. A fresh pass supersedes
    /// the prior pool, even when its earlier candidates were not promoted.
    pub async fn replace(
        &self,
        drafts: Vec<SensingCandidateDraft>,
        actor: InputRoleSnapshot,
    ) -> Result<usize> {
        let now = Utc::now();
        let mut pool = self.pool.write().await;
        pool.candidates.clear();
        let mut appended = 0;
        for draft in drafts.into_iter().take(REVIEW_BATCH_SIZE) {
            let fingerprint = candidate_fingerprint(&draft);
            if pool
                .candidates
                .iter()
                .any(|candidate| candidate.fingerprint == fingerprint)
            {
                continue;
            }
            let sequence = pool.candidates.len();
            pool.candidates.push(SensingCandidate {
                id: format!("sense_{}_{}", now.timestamp_millis(), sequence),
                title: draft.title.trim().to_owned(),
                summary: draft.summary.trim().to_owned(),
                proposed_input: draft.proposed_input.trim().to_owned(),
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
                actor: actor.clone(),
                observed_at: timestamp(now),
                expires_at: timestamp(now + Duration::hours(CANDIDATE_TTL_HOURS)),
                fingerprint,
            });
            appended += 1;
        }
        drop(pool);
        self.persist().await?;
        Ok(appended)
    }

    /// An unavailable intake provider must not leave yesterday's unreviewed
    /// candidates looking like a fresh signal on the next scheduled cycle.
    pub async fn clear(&self) -> Result<()> {
        self.pool.write().await.candidates.clear();
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
            .take(limit.clamp(1, REVIEW_BATCH_SIZE))
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
        InputRoleSnapshot, SensingCandidateDraft, SensingSource, SensingSourceClass, SensingStore,
        format_candidate_pool,
    };

    fn candidate(title: &str, url: &str) -> SensingCandidateDraft {
        SensingCandidateDraft {
            title: title.to_owned(),
            summary: "A short factual change.".to_owned(),
            proposed_input: "A short model input.".to_owned(),
            event_at: None,
            source_class: SensingSourceClass::Research,
            possible_connection: None,
            sources: vec![SensingSource {
                url: url.to_owned(),
                detail: "Primary source.".to_owned(),
            }],
        }
    }

    #[tokio::test]
    async fn a_new_sensing_pass_replaces_the_temporary_candidate_pool() {
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
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].title, "New signal");
        assert_eq!(store.count().await.unwrap(), 1);
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
