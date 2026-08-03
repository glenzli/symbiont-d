use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{
    Actor, ActorType, InitialRelation, LifecycleStatus, PagePayload, Projection, ProvenanceEvent,
    ReadPagesRequest, RevisePageRequest, SearchFilters, SearchMode, SearchPagesRequest,
    SearchTermMatch, SourceRef, WritePageRequest, WriteResult,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::{
    continuity::ContinuityHost,
    reflection::{ConversationEpisode, ReflectionStore},
};

const EPISODE_LIMIT: usize = 1_000;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PcpIndexPhase {
    #[default]
    Idle,
    Syncing,
    Error,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcpIndexSnapshot {
    pub phase: PcpIndexPhase,
    pub episode_pages: u64,
    pub created_pages: u64,
    pub revised_pages: u64,
    pub unchanged_pages: u64,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
}

pub struct PcpIndex {
    continuity: Arc<ContinuityHost>,
    reflection: Arc<ReflectionStore>,
    sync_gate: Mutex<()>,
    snapshot: RwLock<PcpIndexSnapshot>,
}

impl PcpIndex {
    pub fn new(continuity: Arc<ContinuityHost>, reflection: Arc<ReflectionStore>) -> Self {
        Self {
            continuity,
            reflection,
            sync_gate: Mutex::new(()),
            snapshot: RwLock::new(PcpIndexSnapshot::default()),
        }
    }

    pub async fn snapshot(&self) -> PcpIndexSnapshot {
        self.snapshot.read().await.clone()
    }

    pub async fn sync_all(&self) -> Result<PcpIndexSnapshot> {
        let _guard = self.sync_gate.lock().await;
        {
            let mut snapshot = self.snapshot.write().await;
            snapshot.phase = PcpIndexPhase::Syncing;
            snapshot.last_error = None;
        }
        let result = self.sync_all_inner().await;
        match result {
            Ok(snapshot) => {
                *self.snapshot.write().await = snapshot.clone();
                Ok(snapshot)
            }
            Err(error) => {
                let mut snapshot = self.snapshot.write().await;
                snapshot.phase = PcpIndexPhase::Error;
                snapshot.last_sync_at = Some(now());
                snapshot.last_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    async fn sync_all_inner(&self) -> Result<PcpIndexSnapshot> {
        let episodes = self.reflection.episodes(EPISODE_LIMIT).await?;
        let episodes = dependency_order(episodes);
        let mut indexed_revisions = HashMap::<String, String>::new();
        let mut snapshot = PcpIndexSnapshot {
            phase: PcpIndexPhase::Idle,
            episode_pages: episodes.len() as u64,
            last_sync_at: Some(now()),
            ..PcpIndexSnapshot::default()
        };

        for episode in episodes {
            let parent_revision_ids = episode
                .parent_episode_ids
                .iter()
                .filter_map(|id| indexed_revisions.get(id).cloned())
                .collect::<Vec<_>>();
            let outcome = self
                .sync_episode(&episode, &parent_revision_ids)
                .await
                .with_context(|| format!("sync PCP Topic Episode {}", episode.id))?;
            indexed_revisions.insert(episode.id.clone(), outcome.page.revision_id.clone());
            match outcome.kind {
                SyncKind::Created => snapshot.created_pages += 1,
                SyncKind::Revised => snapshot.revised_pages += 1,
                SyncKind::Unchanged => snapshot.unchanged_pages += 1,
            }
        }
        Ok(snapshot)
    }

    async fn sync_episode(
        &self,
        episode: &ConversationEpisode,
        parent_revision_ids: &[String],
    ) -> Result<SyncOutcome> {
        let stable_key = format!("reflection.episode.{}", episode.id);
        let digest = episode_digest(episode)?;
        let content = format!("# {}\n\n{}", episode.title.trim(), episode.summary.trim());
        let facets = json!({
            "kind": "conversation_episode",
            "indexRole": "aggregate",
            "stableKey": stable_key,
            "episodeId": episode.id,
            "title": episode.title,
            "state": episode.state.as_str(),
            "startedAt": episode.started_at,
            "sourceDigest": digest,
        });
        let aggregate_targets = sorted(parent_revision_ids.to_vec());
        let mut source_revision_ids = episode.source_revision_ids.clone();
        source_revision_ids.sort();
        source_revision_ids.dedup();
        let relations = episode_relations(&aggregate_targets, &source_revision_ids);
        let actor = index_actor();
        let mut provenance_inputs = aggregate_targets.clone();
        provenance_inputs.extend(source_revision_ids.iter().cloned());
        provenance_inputs = sorted(provenance_inputs);
        let provenance = vec![ProvenanceEvent {
            operation: "derive".to_owned(),
            actor: actor.clone(),
            timestamp: now(),
            input_revision_ids: provenance_inputs,
            tool_or_model: Some("symbiont Reflection index".to_owned()),
        }];
        let source_refs = vec![SourceRef {
            source_type: "symbiont_topic_episode".to_owned(),
            uri: format!("symbiont://reflection/episodes/{}", episode.id),
            locator: None,
            metadata: None,
        }];

        if let Some(current) = self.current_episode_page(&stable_key).await? {
            if current
                .revision
                .facets
                .as_ref()
                .and_then(|facets| facets.get("sourceDigest"))
                .and_then(Value::as_str)
                == Some(digest.as_str())
            {
                return Ok(SyncOutcome {
                    page: WriteResult {
                        page_id: current.revision.page_id,
                        revision_id: current.revision.revision_id,
                        created: false,
                    },
                    kind: SyncKind::Unchanged,
                });
            }
            let page = self
                .continuity
                .store()
                .revise_page(RevisePageRequest {
                    page_id: current.revision.page_id,
                    expected_revision_id: current.revision.revision_id,
                    created_by: actor,
                    lifecycle_status: LifecycleStatus::Active,
                    observed_at: Some(episode.last_activity_at.clone()),
                    valid_from: None,
                    valid_to: None,
                    payload: Some(PagePayload {
                        media_type: "text/markdown".to_owned(),
                        content,
                    }),
                    source_refs,
                    facets: Some(facets),
                    provenance,
                    initial_relations: relations,
                    idempotency_key: Some(format!("episode-index:{}:{digest}", episode.id)),
                })
                .await?;
            return Ok(SyncOutcome {
                page,
                kind: SyncKind::Revised,
            });
        }

        let page = self
            .continuity
            .store()
            .write_page(WritePageRequest {
                owner_id: self.continuity.store().owner_id().to_owned(),
                namespace: self.continuity.project_scope().to_owned(),
                visibility: "private".to_owned(),
                lifecycle_status: LifecycleStatus::Active,
                created_by: actor,
                observed_at: Some(episode.last_activity_at.clone()),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content,
                }),
                source_refs,
                facets: Some(facets),
                provenance,
                initial_relations: relations,
                idempotency_key: Some(format!("episode-index:create:{}:{digest}", episode.id)),
            })
            .await?;
        Ok(SyncOutcome {
            page,
            kind: SyncKind::Created,
        })
    }

    async fn current_episode_page(&self, stable_key: &str) -> Result<Option<pcp_core::ReadPage>> {
        let revision_ids = self
            .continuity
            .search(SearchPagesRequest {
                query: stable_key.to_owned(),
                scopes: vec![self.continuity.project_scope().to_owned()],
                mode: SearchMode::Exact,
                term_match: SearchTermMatch::All,
                projections: vec![Projection::Facets],
                filters: SearchFilters::default(),
                limit: 50,
                cursor: None,
            })
            .await?
            .hits
            .into_iter()
            .map(|hit| hit.revision_id)
            .collect::<Vec<_>>();
        if revision_ids.is_empty() {
            return Ok(None);
        }
        Ok(self
            .continuity
            .read(ReadPagesRequest {
                revision_ids,
                projections: vec![Projection::Facets],
                max_chars: 16_000,
            })
            .await?
            .into_iter()
            .find(|page| {
                page.revision
                    .facets
                    .as_ref()
                    .and_then(|facets| facets.get("stableKey"))
                    .and_then(Value::as_str)
                    == Some(stable_key)
            }))
    }
}

struct SyncOutcome {
    page: WriteResult,
    kind: SyncKind,
}

enum SyncKind {
    Created,
    Revised,
    Unchanged,
}

fn dependency_order(mut episodes: Vec<ConversationEpisode>) -> Vec<ConversationEpisode> {
    let mut ordered = Vec::with_capacity(episodes.len());
    let mut resolved = std::collections::HashSet::<String>::new();
    while !episodes.is_empty() {
        let before = episodes.len();
        let mut deferred = Vec::new();
        for episode in episodes {
            if episode
                .parent_episode_ids
                .iter()
                .all(|parent| resolved.contains(parent))
            {
                resolved.insert(episode.id.clone());
                ordered.push(episode);
            } else {
                deferred.push(episode);
            }
        }
        if deferred.len() == before {
            deferred.sort_by(|left, right| left.id.cmp(&right.id));
            ordered.extend(deferred);
            break;
        }
        episodes = deferred;
    }
    ordered
}

fn episode_digest(episode: &ConversationEpisode) -> Result<String> {
    let value = json!({
        "title": episode.title,
        "summary": episode.summary,
        "state": episode.state.as_str(),
        "sources": sorted(episode.source_revision_ids.clone()),
        "parents": sorted(episode.parent_episode_ids.clone()),
    });
    let encoded = serde_json::to_vec(&value).context("encode Topic Episode index digest")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn episode_relations(
    aggregate_targets: &[String],
    source_revision_ids: &[String],
) -> Vec<InitialRelation> {
    aggregate_targets
        .iter()
        .map(|revision_id| InitialRelation {
            relation_type: "aggregates".to_owned(),
            to_revision_id: revision_id.clone(),
        })
        .chain(
            source_revision_ids
                .iter()
                .map(|revision_id| InitialRelation {
                    relation_type: "derived_from".to_owned(),
                    to_revision_id: revision_id.clone(),
                }),
        )
        .collect()
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn index_actor() -> Actor {
    Actor {
        actor_type: ActorType::System,
        actor_id: "system:symbiont-reflection-index".to_owned(),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_core::{Projection, ReadPagesRequest};
    use pcp_sqlite::SqlitePcpStore;

    use super::PcpIndex;
    use crate::{
        continuity::{ContinuityHost, MessageLinks},
        memory::MemoryRole,
        reflection::{EpisodeInput, EpisodeState, ReflectionStore},
    };

    #[tokio::test]
    async fn syncs_model_topics_as_traceable_aggregate_pages() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-pcp-index-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP"),
        );
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
        let reflection = Arc::new(
            ReflectionStore::open(
                root.join("reflection.sqlite3"),
                root.join("reflection.toml"),
            )
            .await
            .expect("open Reflection"),
        );
        let user = continuity
            .ingest_message(
                MemoryRole::User,
                "A compact but durable architectural decision.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("write user message");
        reflection
            .record_message(&user.entry, None, false, &[])
            .await
            .expect("record Reflection message");
        let episode = reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Durable architecture".to_owned(),
                summary: "The discussion established a reusable architectural direction."
                    .to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![user.page.revision_id.clone()],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("write Episode");

        let index = PcpIndex::new(Arc::clone(&continuity), reflection);
        let first = index.sync_all().await.expect("sync index");
        assert_eq!(first.created_pages, 1);
        let second = index.sync_all().await.expect("sync index again");
        assert_eq!(second.unchanged_pages, 1);
        let followup = continuity
            .ingest_message(
                MemoryRole::User,
                "A later message belongs to the topic without changing its semantic index.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("write follow-up message");
        index
            .reflection
            .record_message(&followup.entry, None, false, &[])
            .await
            .expect("record follow-up Reflection message");
        index
            .reflection
            .attach_episode_messages(&episode.id, &[followup.page.revision_id])
            .await
            .expect("attach follow-up to Episode");
        let membership_only = index.sync_all().await.expect("sync membership-only change");
        assert_eq!(membership_only.unchanged_pages, 1);
        index
            .reflection
            .upsert_episode(EpisodeInput {
                id: Some(episode.id),
                title: "Durable architecture".to_owned(),
                summary: "The discussion refined the reusable architectural direction.".to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![user.page.revision_id.clone()],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("revise Episode");
        let third = index.sync_all().await.expect("sync revised index");
        assert_eq!(third.revised_pages, 1);

        let browsed = continuity
            .browse_index(&[], 20, None, 8_000)
            .await
            .expect("browse index");
        let episode = browsed
            .hits
            .iter()
            .find(|hit| {
                hit.facets
                    .as_ref()
                    .and_then(|facets| facets.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("conversation_episode")
            })
            .expect("Episode index entry");
        let page = continuity
            .read(ReadPagesRequest {
                revision_ids: vec![episode.revision_id.clone()],
                projections: vec![Projection::Relations],
                max_chars: 8_000,
            })
            .await
            .expect("read Episode relations")
            .remove(0);
        assert!(page.relations.iter().any(|relation| {
            relation.relation_type == "derived_from"
                && relation.to_revision_id == user.page.revision_id
        }));
        assert!(
            !page
                .relations
                .iter()
                .any(|relation| relation.relation_type == "aggregates")
        );

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn stable_episode_keys_do_not_collide_on_prefixes() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-pcp-index-prefix-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP"),
        );
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
        let reflection = Arc::new(
            ReflectionStore::open(
                root.join("reflection.sqlite3"),
                root.join("reflection.toml"),
            )
            .await
            .expect("open Reflection"),
        );
        for (stable_key, title) in [
            ("reflection.episode.ep_prefix_suffix", "Long key"),
            ("reflection.episode.ep_prefix", "Short key"),
        ] {
            continuity
                .write_model_page(
                    Some(continuity.project_scope()),
                    &format!("# {title}\n\nSummary for {title}."),
                    Some(serde_json::json!({
                        "kind": "conversation_episode",
                        "stableKey": stable_key,
                    })),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Some(stable_key.to_owned()),
                )
                .await
                .expect("write indexed Page");
        }

        let index = PcpIndex::new(Arc::clone(&continuity), reflection);
        for stable_key in [
            "reflection.episode.ep_prefix",
            "reflection.episode.ep_prefix_suffix",
        ] {
            let page = index
                .current_episode_page(stable_key)
                .await
                .expect("find exact stable key")
                .expect("indexed Page");
            assert_eq!(
                page.revision
                    .facets
                    .as_ref()
                    .and_then(|facets| facets.get("stableKey"))
                    .and_then(serde_json::Value::as_str),
                Some(stable_key)
            );
        }

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
