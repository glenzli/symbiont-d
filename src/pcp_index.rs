use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{
    Actor, ActorType, LifecycleStatus, PageMutability, PagePayload, Projection, ProvenanceEvent,
    ReadPagesRequest, RevisePageRequest, SearchFilters, SearchMode, SearchPagesRequest,
    SearchTermMatch, SourceRef, WritePageRequest, WriteResult,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

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
    pub skipped_episode_pages: u64,
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
        let aliases = self.reflection.episode_aliases().await?;
        let episode_titles = episodes
            .iter()
            .map(|episode| (episode.id.clone(), episode.title.clone()))
            .collect::<HashMap<_, _>>();
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
            match self.sync_episode(&episode, &parent_revision_ids).await {
                Ok(outcome) => {
                    indexed_revisions.insert(episode.id.clone(), outcome.page.revision_id.clone());
                    match outcome.kind {
                        SyncKind::Created => snapshot.created_pages += 1,
                        SyncKind::Revised => snapshot.revised_pages += 1,
                        SyncKind::Unchanged => snapshot.unchanged_pages += 1,
                    }
                }
                Err(error) if is_derivation_cycle_error(&error) => {
                    snapshot.skipped_episode_pages += 1;
                    warn!(
                        episode_id = %episode.id,
                        "skipping PCP Topic Episode index update because its historical derivation would form a cycle"
                    );
                    if let Some(current) = self
                        .current_episode_page(&format!("reflection.episode.{}", episode.id))
                        .await?
                    {
                        indexed_revisions.insert(episode.id.clone(), current.revision.revision_id);
                    }
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("sync PCP Topic Episode {}", episode.id));
                }
            }
        }
        for alias in aliases {
            let Some(canonical_revision_id) = indexed_revisions.get(&alias.canonical_id) else {
                continue;
            };
            if self
                .retire_alias_page(
                    &alias.alias_id,
                    &alias.canonical_id,
                    episode_titles
                        .get(&alias.canonical_id)
                        .map(String::as_str)
                        .unwrap_or("合并后的主题"),
                    canonical_revision_id,
                )
                .await?
            {
                snapshot.revised_pages += 1;
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
        let source_revision_ids = sorted(episode.source_revision_ids.clone());
        let turn_revision_ids = self
            .reflection
            .conversation_turn_revision_ids(&source_revision_ids)
            .await?;
        let supporting_turn_revision_ids = turn_revision_ids
            .iter()
            .filter(|revision_id| !source_revision_ids.contains(revision_id))
            .cloned()
            .collect::<Vec<_>>();
        let digest = episode_digest(episode, &turn_revision_ids)?;
        let content = format!("# {}\n\n{}", episode.title.trim(), episode.summary.trim());
        let facets = json!({
            "kind": "conversation_episode",
            "indexRole": "aggregate",
            "stableKey": stable_key,
            "episodeId": episode.id,
            "title": episode.title,
            "state": episode.state.as_str(),
            "startedAt": episode.started_at,
            "evidenceRevisionIds": source_revision_ids,
            "supportingTurnRevisionIds": supporting_turn_revision_ids,
            "sourceDigest": digest,
        });
        let parent_targets = sorted(parent_revision_ids.to_vec());
        let relations = self
            .continuity
            .initial_relations_for_revision_targets(
                parent_targets
                    .iter()
                    .cloned()
                    .map(|revision_id| ("continues".to_owned(), revision_id))
                    .chain(
                        supporting_turn_revision_ids
                            .iter()
                            .cloned()
                            .map(|revision_id| ("includes".to_owned(), revision_id)),
                    )
                    .chain(
                        source_revision_ids
                            .iter()
                            .cloned()
                            .map(|revision_id| ("derived_from".to_owned(), revision_id)),
                    )
                    .collect(),
            )
            .await?;
        let actor = index_actor();
        let provenance_inputs = source_revision_ids.clone();
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
                kind: "conversation_episode".to_owned(),
                mutability: PageMutability::Revisioned,
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

    async fn retire_alias_page(
        &self,
        alias_id: &str,
        canonical_id: &str,
        canonical_title: &str,
        canonical_revision_id: &str,
    ) -> Result<bool> {
        let stable_key = format!("reflection.episode.{alias_id}");
        let Some(current) = self.current_episode_page(&stable_key).await? else {
            return Ok(false);
        };
        if current.page.lifecycle_status != LifecycleStatus::Active {
            return Ok(false);
        }
        let mut facets = current.revision.facets.clone().unwrap_or_else(|| json!({}));
        if let Some(object) = facets.as_object_mut() {
            object.insert(
                "indexRole".to_owned(),
                Value::String("aggregate_alias".to_owned()),
            );
            object.insert(
                "canonicalEpisodeId".to_owned(),
                Value::String(canonical_id.to_owned()),
            );
            object.insert("state".to_owned(), Value::String("merged".to_owned()));
        }
        let actor = index_actor();
        let timestamp = now();
        let relations = self
            .continuity
            .initial_relations_for_revision_targets(vec![(
                "outdated_by".to_owned(),
                canonical_revision_id.to_owned(),
            )])
            .await?;
        self.continuity
            .store()
            .revise_page(RevisePageRequest {
                page_id: current.page.page_id,
                expected_revision_id: current.revision.revision_id.clone(),
                created_by: actor.clone(),
                lifecycle_status: LifecycleStatus::Superseded,
                observed_at: Some(timestamp.clone()),
                valid_from: None,
                valid_to: Some(timestamp.clone()),
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: format!("# {canonical_title}\n\n此重复主题已合并至统一主题。"),
                }),
                source_refs: vec![SourceRef {
                    source_type: "symbiont_topic_episode_alias".to_owned(),
                    uri: format!("symbiont://reflection/episodes/{alias_id}"),
                    locator: None,
                    metadata: Some(json!({"canonicalEpisodeId": canonical_id})),
                }],
                facets: Some(facets),
                provenance: vec![ProvenanceEvent {
                    operation: "consolidate".to_owned(),
                    actor,
                    timestamp,
                    input_revision_ids: vec![
                        current.revision.revision_id,
                        canonical_revision_id.to_owned(),
                    ],
                    tool_or_model: Some("symbiont Reflection index".to_owned()),
                }],
                initial_relations: relations,
                idempotency_key: Some(format!(
                    "episode-index:alias:{alias_id}:{canonical_revision_id}"
                )),
            })
            .await?;
        Ok(true)
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
                page_ids: Vec::new(),
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

fn episode_digest(episode: &ConversationEpisode, turn_revision_ids: &[String]) -> Result<String> {
    let value = json!({
        "title": episode.title,
        "summary": episode.summary,
        "state": episode.state.as_str(),
        "sources": sorted(episode.source_revision_ids.clone()),
        "turn": sorted(turn_revision_ids.to_vec()),
        "parents": sorted(episode.parent_episode_ids.clone()),
    });
    let encoded = serde_json::to_vec(&value).context("encode Topic Episode index digest")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn is_derivation_cycle_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .to_string()
            .contains("relation would introduce a cycle in the PCP derivation DAG")
    })
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

    use pcp_core::{LifecycleStatus, Projection, ReadPagesRequest};
    use pcp_sqlite::SqlitePcpStore;
    use rusqlite::{Connection, params};
    use serde_json::json;

    use super::{PcpIndex, now};
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
        let assistant = continuity
            .ingest_message(
                MemoryRole::Assistant,
                "Use the low-cost route for reversible work, with an escalation boundary.",
                Vec::new(),
                None,
                MessageLinks {
                    responds_to: Some(user.page.revision_id.clone()),
                    ..MessageLinks::default()
                },
            )
            .await
            .expect("write assistant message");
        reflection
            .record_message(&assistant.entry, Some(&user.page.revision_id), false, &[])
            .await
            .expect("record assistant Reflection message");
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
                page_ids: Vec::new(),
                revision_ids: vec![episode.revision_id.clone()],
                projections: vec![Projection::Relations],
                max_chars: 8_000,
            })
            .await
            .expect("read Episode relations")
            .remove(0);
        assert!(page.relations.iter().any(|relation| {
            relation.relation_type == "derived_from" && relation.to_page_id == user.page.page_id
        }));
        assert!(page.relations.iter().any(|relation| {
            relation.relation_type == "includes" && relation.to_page_id == assistant.page.page_id
        }));

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

    #[tokio::test]
    async fn retires_projected_pages_for_consolidated_episode_aliases() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-pcp-index-alias-{nonce}"));
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
        let reflection_path = root.join("reflection.sqlite3");
        let reflection = Arc::new(
            ReflectionStore::open(reflection_path.clone(), root.join("reflection.toml"))
                .await
                .expect("open Reflection"),
        );
        let user = continuity
            .ingest_message(
                MemoryRole::User,
                "One topic identity should remain after consolidation.",
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
        let canonical = reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Canonical topic".to_owned(),
                summary: "The duplicate identity has one canonical owner.".to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![user.page.revision_id],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("write canonical Episode");
        let alias_page = continuity
            .write_model_page(
                Some(continuity.project_scope()),
                "# Canonical topic\n\nLegacy duplicate projection.",
                Some(json!({
                    "kind": "conversation_episode",
                    "stableKey": "reflection.episode.ep_legacy_alias",
                    "episodeId": "ep_legacy_alias",
                })),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Some("reflection.episode.ep_legacy_alias".to_owned()),
            )
            .await
            .expect("write legacy alias Page");
        Connection::open(reflection_path)
            .expect("open Reflection database")
            .execute(
                "INSERT INTO episode_aliases (alias_id, canonical_id, merged_at) VALUES (?1, ?2, ?3)",
                params!["ep_legacy_alias", canonical.id, now()],
            )
            .expect("record Episode alias");

        let index = PcpIndex::new(Arc::clone(&continuity), reflection);
        let snapshot = index.sync_all().await.expect("sync consolidated index");
        assert_eq!(snapshot.created_pages, 1);
        assert_eq!(snapshot.revised_pages, 1);
        let alias = continuity
            .read(ReadPagesRequest {
                page_ids: vec![alias_page.page_id],
                revision_ids: Vec::new(),
                projections: vec![Projection::Manifest, Projection::Relations],
                max_chars: 8_000,
            })
            .await
            .expect("read alias relations")
            .remove(0);
        assert_eq!(alias.page.lifecycle_status, LifecycleStatus::Superseded);
        assert!(
            alias
                .relations
                .iter()
                .any(|relation| relation.relation_type == "outdated_by")
        );

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn paired_answer_that_used_topic_context_does_not_block_index_recovery() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-pcp-index-turn-cycle-{nonce}"));
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
                "Refine the current topic using its existing context.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("write user message");
        reflection
            .record_message(&user.entry, None, false, &[])
            .await
            .expect("record user Reflection message");
        let episode = reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Context-aware topic".to_owned(),
                summary: "The topic exists before the direct answer is generated.".to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![user.page.revision_id.clone()],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("write Episode");
        let index = PcpIndex::new(Arc::clone(&continuity), Arc::clone(&reflection));
        index.sync_all().await.expect("sync initial Episode");
        let topic_revision_id = index
            .current_episode_page(&format!("reflection.episode.{}", episode.id))
            .await
            .expect("read Episode Page")
            .expect("Episode Page exists")
            .revision
            .revision_id;
        let assistant = continuity
            .ingest_message(
                MemoryRole::Assistant,
                "The answer can use Topic context without becoming its derivation parent.",
                Vec::new(),
                None,
                MessageLinks {
                    responds_to: Some(user.page.revision_id.clone()),
                    input_revision_ids: vec![topic_revision_id],
                    ..MessageLinks::default()
                },
            )
            .await
            .expect("write context-aware assistant message");
        reflection
            .record_message(&assistant.entry, Some(&user.page.revision_id), false, &[])
            .await
            .expect("record assistant Reflection message");

        let refreshed = index.sync_all().await.expect("sync paired turn");
        assert_eq!(refreshed.revised_pages, 1);
        let topic = index
            .current_episode_page(&format!("reflection.episode.{}", episode.id))
            .await
            .expect("read refreshed Episode Page")
            .expect("refreshed Episode Page exists");
        let preserved_topic_revision_id = topic.revision.revision_id.clone();
        let topic = continuity
            .read(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: vec![topic.revision.revision_id],
                projections: vec![Projection::Relations],
                max_chars: 8_000,
            })
            .await
            .expect("read refreshed Episode relations")
            .remove(0);
        assert!(topic.relations.iter().any(|relation| {
            relation.relation_type == "includes" && relation.to_page_id == assistant.page.page_id
        }));

        index
            .reflection
            .upsert_episode(EpisodeInput {
                id: Some(episode.id.clone()),
                title: "Context-aware topic".to_owned(),
                summary: "The topic now cites an answer that used its earlier projection."
                    .to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![
                    user.page.revision_id.clone(),
                    assistant.page.revision_id.clone(),
                ],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("record cyclic historical evidence");
        let recovered = index
            .sync_all()
            .await
            .expect("skip one cyclic Episode projection instead of blocking the index");
        assert_eq!(recovered.skipped_episode_pages, 1);
        let preserved = index
            .current_episode_page(&format!("reflection.episode.{}", episode.id))
            .await
            .expect("read preserved Episode Page")
            .expect("preserved Episode Page exists");
        assert_eq!(preserved.revision.revision_id, preserved_topic_revision_id);

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
