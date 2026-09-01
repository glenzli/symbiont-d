use std::{
    collections::HashMap,
    io::ErrorKind,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{fs, sync::RwLock, task};

use super::{
    ConversationEpisode, DeferredFollowUp, EpisodeAlias, EpisodeInput, EpisodeState, FollowUpInput,
    HunchFeedbackTarget, HypothesisHorizon, HypothesisInput, HypothesisStatus, InteractionEvent,
    ProjectionHealth, ReflectionConfig, ReflectionRun, TurnDisposition, WorkingHypothesis,
    lifecycle,
};
use crate::memory::{MemoryEntry, MemoryRole};

const MAX_EVENT_EXCERPT_CHARS: usize = 1_200;
const MAX_PROMPT_CHARS: usize = 24_000;
const MAX_DAILY_TOKEN_LIMIT: u64 = 100_000_000;
static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct ReflectionStore {
    path: PathBuf,
    config_path: PathBuf,
    config: RwLock<ReflectionConfig>,
}

#[derive(Clone, Debug)]
pub struct ReflectionBatch {
    pub from_event_id: i64,
    pub to_event_id: i64,
    pub events: Vec<InteractionEvent>,
    pub source_bundle: String,
    pub lifecycle_audit: bool,
}

impl ReflectionStore {
    pub async fn open(path: PathBuf, config_path: PathBuf) -> Result<Self> {
        let config = read_config(&config_path).await?;
        validate_config(&config)?;
        let path_for_open = path.clone();
        task::spawn_blocking(move || initialize_database(&path_for_open))
            .await
            .context("join Reflection database initialization")??;
        let store = Self {
            path,
            config_path,
            config: RwLock::new(config),
        };
        store.persist_config().await?;
        Ok(store)
    }

    pub async fn config(&self) -> ReflectionConfig {
        self.config.read().await.clone()
    }

    pub async fn update_config(&self, config: ReflectionConfig) -> Result<ReflectionConfig> {
        validate_config(&config)?;
        let content = toml::to_string_pretty(&config).context("encode Reflection config")?;
        persist_file(&self.config_path, &content).await?;
        *self.config.write().await = config.clone();
        Ok(config)
    }

    pub async fn backfill_messages(&self, entries: &[MemoryEntry]) -> Result<()> {
        let mut previous_assistant = None;
        for entry in entries {
            let related = match entry.role {
                MemoryRole::User => previous_assistant.as_deref(),
                MemoryRole::Assistant => None,
                MemoryRole::Memory => continue,
            };
            self.record_message(entry, related, true, &[]).await?;
            if matches!(entry.role, MemoryRole::Assistant) {
                previous_assistant = entry.revision_id.clone();
            }
        }
        Ok(())
    }

    pub async fn record_message(
        &self,
        entry: &MemoryEntry,
        related_revision_id: Option<&str>,
        imported: bool,
        hunch_feedback: &[HunchFeedbackTarget],
    ) -> Result<()> {
        let Some(revision_id) = entry.revision_id.clone() else {
            return Ok(());
        };
        let role = match entry.role {
            MemoryRole::User => "user",
            MemoryRole::Assistant => "assistant",
            MemoryRole::Memory => return Ok(()),
        };
        let kind = format!("message_{role}");
        let at = entry.at.clone();
        let content_chars = entry.content.chars().count() as u64;
        let excerpt = truncate(&entry.content, MAX_EVENT_EXCERPT_CHARS);
        let path = self.path.clone();
        let related_revision_id = related_revision_id.map(str::to_owned);
        let hunch_feedback = hunch_feedback.to_vec();
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            let timing = if role == "user" {
                reply_timing(&connection, related_revision_id.as_deref(), &at)?
            } else {
                Value::Null
            };
            let payload = json!({
                "excerpt": excerpt,
                "imported": imported,
                "replyTiming": timing,
                "hunchFeedback": hunch_feedback
            });
            connection
                .execute(
                    "
                    INSERT OR IGNORE INTO conversation_events (
                        kind, occurred_at, revision_id, related_revision_id,
                        role, content_chars, payload_json, retracted
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
                    ",
                    params![
                        kind,
                        at,
                        revision_id,
                        related_revision_id,
                        role,
                        content_chars as i64,
                        serde_json::to_string(&payload)?
                    ],
                )
                .context("record conversation message event")?;
            Ok(())
        })
        .await
        .context("join message event write")?
    }

    pub async fn record_turn_disposition(
        &self,
        revision_id: &str,
        reaction: Option<&str>,
    ) -> Result<()> {
        if revision_id.trim().is_empty() || revision_id.len() > 128 {
            anyhow::bail!("invalid turn disposition Revision ID");
        }
        let path = self.path.clone();
        let revision_id = revision_id.to_owned();
        let reaction = reaction.map(str::to_owned);
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM conversation_events
                 WHERE revision_id = ?1 AND kind IN ('turn_settled', 'turn_reaction')",
                params![revision_id],
            )?;
            let kind = if reaction.is_some() {
                "turn_reaction"
            } else {
                "turn_settled"
            };
            transaction.execute(
                "
                INSERT INTO conversation_events (
                    kind, occurred_at, revision_id, role,
                    content_chars, payload_json, retracted
                ) VALUES (?1, ?2, ?3, 'assistant', 0, ?4, 0)
                ",
                params![
                    kind,
                    now(),
                    revision_id,
                    serde_json::to_string(&json!({"reaction": reaction}))?
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join turn disposition write")?
    }

    pub async fn recent_turn_dispositions(&self, limit: usize) -> Result<Vec<TurnDisposition>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Vec<TurnDisposition>> {
            let connection = open_connection(&path)?;
            let mut statement = connection.prepare(
                "
                SELECT revision_id, payload_json
                FROM conversation_events
                WHERE kind IN ('turn_settled', 'turn_reaction')
                  AND revision_id IS NOT NULL
                  AND retracted = 0
                ORDER BY id DESC
                LIMIT ?1
                ",
            )?;
            statement
                .query_map(params![limit.clamp(1, 500) as i64], |row| {
                    let revision_id: String = row.get(0)?;
                    let payload: String = row.get(1)?;
                    let payload =
                        serde_json::from_str::<Value>(&payload).unwrap_or_else(|_| json!({}));
                    Ok(TurnDisposition {
                        revision_id,
                        reaction: payload
                            .get("reaction")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
        .context("join recent turn disposition read")?
    }

    pub async fn record_seen(&self, revision_ids: Vec<String>, occurred_at: String) -> Result<()> {
        if !self.config().await.capture_read_state || revision_ids.is_empty() {
            return Ok(());
        }
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            for revision_id in revision_ids {
                transaction.execute(
                    "
                    INSERT OR IGNORE INTO conversation_events (
                        kind, occurred_at, revision_id, role,
                        content_chars, payload_json, retracted
                    ) VALUES ('message_seen', ?1, ?2, 'assistant', 0, '{}', 0)
                    ",
                    params![occurred_at, revision_id],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join message-seen write")?
    }

    pub async fn record_retraction(&self, revision_ids: &[String]) -> Result<()> {
        if revision_ids.is_empty() {
            return Ok(());
        }
        let path = self.path.clone();
        let revision_ids = revision_ids.to_vec();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            for revision_id in revision_ids {
                transaction.execute(
                    "UPDATE conversation_events SET retracted = 1 WHERE revision_id = ?1",
                    params![revision_id],
                )?;
                transaction.execute(
                    "DELETE FROM episode_messages WHERE revision_id = ?1",
                    params![revision_id],
                )?;
                transaction.execute(
                    "
                    INSERT INTO conversation_events (
                        kind, occurred_at, revision_id, content_chars,
                        payload_json, retracted
                    ) VALUES ('message_retracted', ?1, ?2, 0, '{}', 0)
                    ",
                    params![now(), revision_id],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join retraction observation write")?
    }

    pub async fn pending_count(&self) -> Result<u64> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<u64> {
            let connection = open_connection(&path)?;
            let cursor = read_cursor(&connection)?;
            let events = read_events_after(&connection, cursor, 100)?;
            Ok(actionable_prefix(events, 100).len() as u64)
        })
        .await
        .context("join pending Reflection count")?
    }

    pub async fn pending_batch(&self, limit: usize) -> Result<Option<ReflectionBatch>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Option<ReflectionBatch>> {
            let connection = open_connection(&path)?;
            let cursor = read_cursor(&connection)?;
            let events = actionable_prefix(
                read_events_after(&connection, cursor, 100)?,
                limit.clamp(1, 100),
            );
            let Some(first) = events.first() else {
                return Ok(None);
            };
            let from_event_id = first.id;
            let to_event_id = events.last().map(|event| event.id).unwrap_or(from_event_id);
            let episodes = read_episodes(&connection, 12)?;
            let hypotheses = read_hypotheses(&connection, 16)?;
            let follow_ups = read_follow_ups(&connection, 10)?;
            let source_bundle = format_source_bundle(&events, &episodes, &hypotheses, &follow_ups);
            Ok(Some(ReflectionBatch {
                from_event_id,
                to_event_id,
                events,
                source_bundle,
                lifecycle_audit: false,
            }))
        })
        .await
        .context("join pending Reflection batch read")?
    }

    pub async fn lifecycle_batch(&self) -> Result<Option<ReflectionBatch>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Option<ReflectionBatch>> {
            let connection = open_connection(&path)?;
            let cursor = read_cursor(&connection)?;
            let episodes = read_episodes(&connection, 40)?;
            let hypotheses = read_hypotheses(&connection, 60)?;
            let follow_ups = read_follow_ups(&connection, 30)?;
            if episodes.is_empty() && hypotheses.is_empty() && follow_ups.is_empty() {
                return Ok(None);
            }
            let temporal = format_temporal_prompt(&[], &episodes, &hypotheses, &follow_ups);
            let source_bundle = truncate(
                &format!(
                    "<projection-lifecycle-audit>\nNo new conversation event is being interpreted. Review the existing projections themselves. Every active hypothesis must either receive an honest future revisit_after appropriate to its horizon, or be revised to stale, superseded, or contradicted. Do not preserve a hypothesis merely because it once had evidence. Move Topics to dormant or closed only when their own source history shows the line is no longer active; never merge merely adjacent subjects. Preserve exact evidence Revisions and prefer no semantic rewrite when only lifecycle state needs attention.\n</projection-lifecycle-audit>\n\n{temporal}"
                ),
                MAX_PROMPT_CHARS,
            );
            Ok(Some(ReflectionBatch {
                from_event_id: cursor,
                to_event_id: cursor,
                events: Vec::new(),
                source_bundle,
                lifecycle_audit: true,
            }))
        })
        .await
        .context("join projection lifecycle batch read")?
    }

    pub async fn projection_health(&self) -> Result<ProjectionHealth> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<ProjectionHealth> {
            let connection = open_connection(&path)?;
            read_projection_health(&connection, Utc::now())
        })
        .await
        .context("join projection health read")?
    }

    pub async fn lifecycle_review_due(&self) -> Result<bool> {
        let health = self.projection_health().await?;
        Ok(lifecycle::review_due(&health, Utc::now()))
    }

    pub async fn mark_lifecycle_reviewed(&self) -> Result<()> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            connection.execute(
                "
                INSERT INTO reflection_meta (key, value)
                VALUES ('last_lifecycle_review_at', ?1)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                ",
                params![now()],
            )?;
            Ok(())
        })
        .await
        .context("join lifecycle review checkpoint write")?
    }

    pub async fn upsert_episode(&self, input: EpisodeInput) -> Result<ConversationEpisode> {
        validate_sources(&input.source_revision_ids)?;
        if input.title.trim().is_empty() || input.summary.trim().is_empty() {
            anyhow::bail!("episode title and summary are required");
        }
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<ConversationEpisode> {
            let mut connection = open_connection(&path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source_revision_ids = dedup(input.source_revision_ids);
            let parent_episode_ids = dedup(
                input
                    .parent_episode_ids
                    .into_iter()
                    .map(|id| resolve_episode_alias(&transaction, &id))
                    .collect::<Result<Vec<_>>>()?,
            );
            ensure_known_source_revisions(&transaction, &source_revision_ids)?;
            let (id, title) = match input.id {
                Some(id) => {
                    let id = resolve_episode_alias(&transaction, &id)?;
                    ensure_episode_exists(&transaction, &id)?;
                    (id, input.title.trim().to_owned())
                }
                None => match find_episode_id_by_title(&transaction, input.title.trim())? {
                    Some(id) => {
                        let title = transaction.query_row(
                            "SELECT title FROM episodes WHERE id = ?1",
                            params![id],
                            |row| row.get::<_, String>(0),
                        )?;
                        (id, title)
                    }
                    None => (new_id("ep"), input.title.trim().to_owned()),
                },
            };
            ensure_episode_title_is_available(&transaction, &title, &id)?;
            validate_episode_parents(&transaction, &id, &parent_episode_ids)?;
            let existing_started_at = transaction
                .query_row(
                    "SELECT started_at FROM episodes WHERE id = ?1",
                    params![&id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let now = now();
            let started_at = existing_started_at.unwrap_or_else(|| {
                source_time(&transaction, &source_revision_ids, false)
                    .unwrap_or_else(|| now.clone())
            });
            let last_activity_at = source_time(&transaction, &source_revision_ids, true)
                .unwrap_or_else(|| now.clone());
            transaction.execute(
                "
                INSERT INTO episodes (
                    id, title, summary, state, started_at, last_activity_at,
                    updated_at, source_revision_ids_json, related_episode_ids_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    summary = excluded.summary,
                    state = excluded.state,
                    last_activity_at = excluded.last_activity_at,
                    updated_at = excluded.updated_at,
                    source_revision_ids_json = excluded.source_revision_ids_json,
                    related_episode_ids_json = excluded.related_episode_ids_json
                ",
                params![
                    id,
                    title,
                    input.summary.trim(),
                    input.state.as_str(),
                    started_at,
                    last_activity_at,
                    now,
                    serde_json::to_string(&source_revision_ids)?,
                    serde_json::to_string(&parent_episode_ids)?
                ],
            )?;
            let turn_revision_ids = expand_conversation_turns(&transaction, &source_revision_ids)?;
            insert_episode_messages(&transaction, &id, &turn_revision_ids, &now, "reflection")?;
            let episode = read_episode(&transaction, &id)?.context("read written Episode")?;
            transaction.commit()?;
            Ok(episode)
        })
        .await
        .context("join Episode upsert")?
    }

    pub async fn upsert_hypothesis(&self, input: HypothesisInput) -> Result<WorkingHypothesis> {
        validate_sources(&input.source_revision_ids)?;
        if input.statement.trim().is_empty() || input.evidence.trim().is_empty() {
            anyhow::bail!("hypothesis statement and evidence are required");
        }
        lifecycle::validate_hypothesis_review_window(&input, Utc::now())?;
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<WorkingHypothesis> {
            let connection = open_connection(&path)?;
            ensure_known_source_revisions(&connection, &input.source_revision_ids)?;
            let id = match input.id {
                Some(id) => {
                    ensure_hypothesis_exists(&connection, &id)?;
                    id
                }
                None => {
                    ensure_hypothesis_statement_is_new(&connection, input.statement.trim())?;
                    new_id("hyp")
                }
            };
            connection.execute(
                "
                INSERT INTO hypotheses (
                    id, statement, evidence, alternatives, status, horizon,
                    revisit_after, updated_at, source_revision_ids_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(id) DO UPDATE SET
                    statement = excluded.statement,
                    evidence = excluded.evidence,
                    alternatives = excluded.alternatives,
                    status = excluded.status,
                    horizon = excluded.horizon,
                    revisit_after = excluded.revisit_after,
                    updated_at = excluded.updated_at,
                    source_revision_ids_json = excluded.source_revision_ids_json
                ",
                params![
                    id,
                    input.statement.trim(),
                    input.evidence.trim(),
                    input.alternatives.trim(),
                    input.status.as_str(),
                    input.horizon.as_str(),
                    input.revisit_after,
                    now(),
                    serde_json::to_string(&dedup(input.source_revision_ids))?
                ],
            )?;
            read_hypothesis(&connection, &id)?.context("read written hypothesis")
        })
        .await
        .context("join hypothesis upsert")?
    }

    pub async fn schedule_follow_up(&self, input: FollowUpInput) -> Result<DeferredFollowUp> {
        validate_sources(&input.source_revision_ids)?;
        let not_before = DateTime::parse_from_rfc3339(&input.not_before)
            .context("follow-up not_before must be RFC 3339")?
            .with_timezone(&Utc);
        let now_at = Utc::now();
        if not_before < now_at + Duration::minutes(1) {
            anyhow::bail!("follow-up must be at least one minute in the future");
        }
        if not_before > now_at + Duration::days(30) {
            anyhow::bail!("follow-up cannot be scheduled more than 30 days ahead");
        }
        if input.reason.trim().is_empty() {
            anyhow::bail!("follow-up reason is required");
        }
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<DeferredFollowUp> {
            let connection = open_connection(&path)?;
            ensure_known_source_revisions(&connection, &input.source_revision_ids)?;
            let id = new_id("follow");
            let now = now();
            connection.execute(
                "
                INSERT INTO follow_ups (
                    id, reason, not_before, status, created_at, updated_at,
                    source_revision_ids_json
                ) VALUES (?1, ?2, ?3, 'pending', ?4, ?4, ?5)
                ",
                params![
                    id,
                    input.reason.trim(),
                    not_before.to_rfc3339_opts(SecondsFormat::Millis, true),
                    now,
                    serde_json::to_string(&dedup(input.source_revision_ids))?
                ],
            )?;
            read_follow_up(&connection, &id)?.context("read scheduled follow-up")
        })
        .await
        .context("join follow-up scheduling")?
    }

    pub async fn prompt(&self) -> Result<String> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<String> {
            let connection = open_connection(&path)?;
            let events = read_recent_events(&connection, 8)?;
            let episodes = read_episodes(&connection, 8)?;
            let hypotheses = read_hypotheses(&connection, 10)?;
            let follow_ups = read_follow_ups(&connection, 6)?;
            Ok(truncate(
                &format_temporal_prompt(&events, &episodes, &hypotheses, &follow_ups),
                MAX_PROMPT_CHARS,
            ))
        })
        .await
        .context("join Reflection prompt read")?
    }

    pub async fn episodes(&self, limit: usize) -> Result<Vec<ConversationEpisode>> {
        let path = self.path.clone();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            read_episodes(&connection, limit)
        })
        .await
        .context("join Episode read")?
    }

    pub async fn episode(&self, id: &str) -> Result<Option<ConversationEpisode>> {
        let path = self.path.clone();
        let id = id.to_owned();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            let id = resolve_episode_alias(&connection, &id)?;
            read_episode(&connection, &id)
        })
        .await
        .context("join Episode read")?
    }

    pub async fn episode_aliases(&self) -> Result<Vec<EpisodeAlias>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Vec<EpisodeAlias>> {
            let connection = open_connection(&path)?;
            let mut statement = connection.prepare(
                "SELECT alias_id, canonical_id FROM episode_aliases ORDER BY merged_at, alias_id",
            )?;
            Ok(statement
                .query_map([], |row| {
                    Ok(EpisodeAlias {
                        alias_id: row.get(0)?,
                        canonical_id: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .context("join Episode alias read")?
    }

    pub async fn episode_message_counts(&self) -> Result<HashMap<String, u64>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<HashMap<String, u64>> {
            let connection = open_connection(&path)?;
            let mut statement = connection
                .prepare("SELECT episode_id, COUNT(*) FROM episode_messages GROUP BY episode_id")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?;
            Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
        .await
        .context("join Episode message-count read")?
    }

    pub async fn episode_revision_ids(&self, id: &str, limit: usize) -> Result<Vec<String>> {
        let path = self.path.clone();
        let id = id.to_owned();
        task::spawn_blocking(move || -> Result<Vec<String>> {
            let connection = open_connection(&path)?;
            let id = resolve_episode_alias(&connection, &id)?;
            read_episode_revision_ids(&connection, &id, limit)
        })
        .await
        .context("join Episode message read")?
    }

    pub async fn episode_revision_ids_by_topic(
        &self,
        episode_ids: &[String],
        limit_per_topic: usize,
    ) -> Result<HashMap<String, Vec<String>>> {
        let path = self.path.clone();
        let episode_ids = episode_ids.to_vec();
        task::spawn_blocking(move || -> Result<HashMap<String, Vec<String>>> {
            let connection = open_connection(&path)?;
            let mut memberships = HashMap::new();
            for id in episode_ids {
                let id = resolve_episode_alias(&connection, &id)?;
                memberships.insert(
                    id.clone(),
                    read_episode_revision_ids(&connection, &id, limit_per_topic)?,
                );
            }
            Ok(memberships)
        })
        .await
        .context("join Topic membership read")?
    }

    pub async fn conversation_turn_revision_ids(
        &self,
        revision_ids: &[String],
    ) -> Result<Vec<String>> {
        validate_sources(revision_ids)?;
        let path = self.path.clone();
        let revision_ids = revision_ids.to_vec();
        task::spawn_blocking(move || -> Result<Vec<String>> {
            let connection = open_connection(&path)?;
            ensure_known_source_revisions(&connection, &revision_ids)?;
            expand_conversation_turns(&connection, &revision_ids)
        })
        .await
        .context("join conversation turn expansion")?
    }

    pub async fn attach_episode_messages(&self, id: &str, revision_ids: &[String]) -> Result<()> {
        if revision_ids.is_empty() {
            return Ok(());
        }
        validate_sources(revision_ids)?;
        let path = self.path.clone();
        let id = id.to_owned();
        let revision_ids = dedup(revision_ids.to_vec());
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let id = resolve_episode_alias(&connection, &id)?;
            if read_episode(&connection, &id)?.is_none() {
                anyhow::bail!("unknown conversation Topic: {id}");
            }
            ensure_known_source_revisions(&connection, &revision_ids)?;
            let transaction = connection.transaction()?;
            let associated_at = now();
            let turn_revision_ids = expand_conversation_turns(&transaction, &revision_ids)?;
            insert_episode_messages(
                &transaction,
                &id,
                &turn_revision_ids,
                &associated_at,
                "explicit_context",
            )?;
            transaction.execute(
                "UPDATE episodes SET last_activity_at = ?2, updated_at = ?2 WHERE id = ?1",
                params![id, associated_at],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join Episode message attachment")?
    }

    pub async fn hypotheses(&self, limit: usize) -> Result<Vec<WorkingHypothesis>> {
        let path = self.path.clone();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            read_hypotheses(&connection, limit)
        })
        .await
        .context("join hypothesis read")?
    }

    pub async fn follow_ups(&self, limit: usize) -> Result<Vec<DeferredFollowUp>> {
        let path = self.path.clone();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            read_follow_ups(&connection, limit)
        })
        .await
        .context("join follow-up read")?
    }

    pub async fn recent_runs(&self, limit: usize) -> Result<Vec<ReflectionRun>> {
        let path = self.path.clone();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            read_runs(&connection, limit)
        })
        .await
        .context("join Reflection run read")?
    }

    pub async fn ensure_known_revisions(&self, revision_ids: &[String]) -> Result<()> {
        validate_sources(revision_ids)?;
        let path = self.path.clone();
        let revision_ids = revision_ids.to_vec();
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            ensure_known_source_revisions(&connection, &revision_ids)
        })
        .await
        .context("join Reflection source validation")?
    }

    pub async fn unknown_revisions(&self, revision_ids: &[String]) -> Result<Vec<String>> {
        validate_sources(revision_ids)?;
        let path = self.path.clone();
        let revision_ids = dedup(revision_ids.to_vec());
        task::spawn_blocking(move || -> Result<Vec<String>> {
            let connection = open_connection(&path)?;
            let mut unknown = Vec::new();
            for revision_id in revision_ids {
                if !is_known_source_revision(&connection, &revision_id)? {
                    unknown.push(revision_id);
                }
            }
            Ok(unknown)
        })
        .await
        .context("join unknown Reflection source read")?
    }

    pub async fn register_verified_revisions(&self, revision_ids: &[String]) -> Result<()> {
        if revision_ids.is_empty() {
            return Ok(());
        }
        validate_sources(revision_ids)?;
        let path = self.path.clone();
        let revision_ids = dedup(revision_ids.to_vec());
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            for revision_id in revision_ids {
                transaction.execute(
                    "
                    INSERT OR IGNORE INTO verified_revisions (revision_id, verified_at)
                    VALUES (?1, ?2)
                    ",
                    params![revision_id, now()],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join verified Reflection source write")?
    }

    pub async fn start_run(&self, trigger: &str, batch: &ReflectionBatch) -> Result<String> {
        let path = self.path.clone();
        let id = new_id("reflect");
        let result_id = id.clone();
        let trigger = trigger.to_owned();
        let from = batch.from_event_id;
        let to = batch.to_event_id;
        let count = batch.events.len() as u64;
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            connection.execute(
                "
                INSERT INTO reflection_runs (
                    id, trigger, status, started_at, from_event_id,
                    to_event_id, event_count, total_tokens, actions_json
                ) VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6, 0, '[]')
                ",
                params![id, trigger, now(), from, to, count as i64],
            )?;
            Ok(())
        })
        .await
        .context("join Reflection run start")??;
        Ok(result_id)
    }

    pub async fn complete_run(
        &self,
        run_id: &str,
        summary: Option<String>,
        trace_id: Option<String>,
        model: Option<String>,
        total_tokens: u64,
        actions: Vec<String>,
        to_event_id: i64,
    ) -> Result<()> {
        let path = self.path.clone();
        let run_id = run_id.to_owned();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "
                UPDATE reflection_runs SET
                    status = 'completed',
                    completed_at = ?2,
                    summary = ?3,
                    trace_id = ?4,
                    model = ?5,
                    total_tokens = ?6,
                    actions_json = ?7
                WHERE id = ?1
                ",
                params![
                    run_id,
                    now(),
                    summary,
                    trace_id,
                    model,
                    total_tokens as i64,
                    serde_json::to_string(&actions)?
                ],
            )?;
            transaction.execute(
                "
                INSERT INTO reflection_meta (key, value) VALUES ('cursor', ?1)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                ",
                params![to_event_id.to_string()],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join Reflection run completion")?
    }

    pub async fn fail_run(&self, run_id: &str, error: &str) -> Result<()> {
        let path = self.path.clone();
        let run_id = run_id.to_owned();
        let error = truncate(error, 4_000);
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            connection.execute(
                "
                UPDATE reflection_runs SET
                    status = 'error', completed_at = ?2, error = ?3
                WHERE id = ?1
                ",
                params![run_id, now(), error],
            )?;
            Ok(())
        })
        .await
        .context("join Reflection run failure")?
    }

    pub async fn interrupt_run(&self, run_id: &str, reason: &str) -> Result<()> {
        let path = self.path.clone();
        let run_id = run_id.to_owned();
        let reason = truncate(reason, 4_000);
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            connection.execute(
                "
                UPDATE reflection_runs SET
                    status = 'interrupted', completed_at = ?2, error = ?3
                WHERE id = ?1
                ",
                params![run_id, now(), reason],
            )?;
            Ok(())
        })
        .await
        .context("join Reflection run interruption")?
    }

    pub async fn due_follow_ups(&self) -> Result<Vec<DeferredFollowUp>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Vec<DeferredFollowUp>> {
            let connection = open_connection(&path)?;
            let mut statement = connection.prepare(
                "
                SELECT id, reason, not_before, status, created_at, updated_at,
                       triggered_at, completed_at, outcome, source_revision_ids_json
                FROM follow_ups
                WHERE status = 'pending' AND not_before <= ?1
                ORDER BY not_before
                LIMIT 3
                ",
            )?;
            statement
                .query_map(params![now()], follow_up_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
        .context("join due follow-up read")?
    }

    pub async fn mark_follow_ups_triggered(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let path = self.path.clone();
        let ids = ids.to_vec();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            for id in ids {
                transaction.execute(
                    "
                    UPDATE follow_ups SET
                        status = 'triggered', triggered_at = ?2, updated_at = ?2
                    WHERE id = ?1 AND status = 'pending'
                    ",
                    params![id, now()],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join follow-up trigger update")?
    }

    pub async fn cancel_follow_ups(&self, ids: &[String], outcome: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let path = self.path.clone();
        let ids = ids.to_vec();
        let outcome = outcome.to_owned();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            let now = now();
            for id in ids {
                transaction.execute(
                    "
                    UPDATE follow_ups SET
                        status = 'canceled', completed_at = ?1,
                        updated_at = ?1, outcome = ?2
                    WHERE id = ?3 AND status = 'pending'
                    ",
                    params![now, outcome, id],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join follow-up cancellation write")?
    }

    pub async fn complete_triggered_follow_ups(&self, outcome: &str) -> Result<()> {
        let path = self.path.clone();
        let outcome = outcome.to_owned();
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            let now = now();
            connection.execute(
                "
                UPDATE follow_ups SET
                    status = 'completed', completed_at = ?1,
                    updated_at = ?1, outcome = ?2
                WHERE status = 'triggered'
                ",
                params![now, outcome],
            )?;
            Ok(())
        })
        .await
        .context("join follow-up completion")?
    }

    pub async fn prune(&self) -> Result<()> {
        let config = self.config().await;
        let cutoff = (Utc::now() - Duration::days(config.retention_days as i64))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<()> {
            let connection = open_connection(&path)?;
            connection.execute(
                "
                DELETE FROM conversation_events
                WHERE occurred_at < ?1
                  AND id <= CAST(COALESCE(
                      (SELECT value FROM reflection_meta WHERE key = 'cursor'),
                      '0'
                  ) AS INTEGER)
                ",
                params![cutoff],
            )?;
            connection.execute(
                "
                DELETE FROM reflection_runs
                WHERE completed_at IS NOT NULL
                  AND completed_at < datetime('now', '-30 days')
                  AND id NOT IN (
                      SELECT id FROM reflection_runs
                      ORDER BY started_at DESC LIMIT 30
                  )
                ",
                [],
            )?;
            Ok(())
        })
        .await
        .context("join Reflection pruning")?
    }

    async fn persist_config(&self) -> Result<()> {
        let content = toml::to_string_pretty(&*self.config.read().await)
            .context("encode Reflection config")?;
        persist_file(&self.config_path, &content).await
    }
}

fn initialize_database(path: &PathBuf) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create Reflection directory {}", parent.display()))?;
    }
    let mut connection = Connection::open(path)
        .with_context(|| format!("open Reflection database {}", path.display()))?;
    connection.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        CREATE TABLE IF NOT EXISTS conversation_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            revision_id TEXT,
            related_revision_id TEXT,
            role TEXT,
            content_chars INTEGER NOT NULL DEFAULT 0,
            payload_json TEXT NOT NULL DEFAULT '{}',
            retracted INTEGER NOT NULL DEFAULT 0
        );
        CREATE UNIQUE INDEX IF NOT EXISTS conversation_event_identity
            ON conversation_events(kind, revision_id)
            WHERE revision_id IS NOT NULL
              AND kind IN ('message_user', 'message_assistant', 'message_seen');
        CREATE INDEX IF NOT EXISTS conversation_event_time
            ON conversation_events(occurred_at, id);
        CREATE UNIQUE INDEX IF NOT EXISTS conversation_turn_disposition_identity
            ON conversation_events(revision_id)
            WHERE revision_id IS NOT NULL
              AND kind IN ('turn_settled', 'turn_reaction');
        CREATE TABLE IF NOT EXISTS reflection_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        INSERT OR IGNORE INTO reflection_meta (key, value) VALUES ('cursor', '0');
        CREATE TABLE IF NOT EXISTS verified_revisions (
            revision_id TEXT PRIMARY KEY,
            verified_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS episodes (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            summary TEXT NOT NULL,
            state TEXT NOT NULL,
            started_at TEXT NOT NULL,
            last_activity_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            source_revision_ids_json TEXT NOT NULL,
            related_episode_ids_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS episode_activity
            ON episodes(last_activity_at DESC);
        CREATE TABLE IF NOT EXISTS episode_messages (
            episode_id TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            associated_at TEXT NOT NULL,
            association_source TEXT NOT NULL,
            PRIMARY KEY (episode_id, revision_id)
        );
        CREATE INDEX IF NOT EXISTS episode_message_revision
            ON episode_messages(revision_id);
        CREATE TABLE IF NOT EXISTS episode_aliases (
            alias_id TEXT PRIMARY KEY,
            canonical_id TEXT NOT NULL,
            merged_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS episode_alias_canonical
            ON episode_aliases(canonical_id);
        CREATE TABLE IF NOT EXISTS hypotheses (
            id TEXT PRIMARY KEY,
            statement TEXT NOT NULL,
            evidence TEXT NOT NULL,
            alternatives TEXT NOT NULL,
            status TEXT NOT NULL,
            horizon TEXT NOT NULL,
            revisit_after TEXT,
            updated_at TEXT NOT NULL,
            source_revision_ids_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS hypothesis_updated
            ON hypotheses(updated_at DESC);
        CREATE TABLE IF NOT EXISTS follow_ups (
            id TEXT PRIMARY KEY,
            reason TEXT NOT NULL,
            not_before TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            triggered_at TEXT,
            completed_at TEXT,
            outcome TEXT,
            source_revision_ids_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS follow_up_due
            ON follow_ups(status, not_before);
        CREATE TABLE IF NOT EXISTS reflection_runs (
            id TEXT PRIMARY KEY,
            trigger TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            completed_at TEXT,
            from_event_id INTEGER,
            to_event_id INTEGER,
            event_count INTEGER NOT NULL,
            summary TEXT,
            trace_id TEXT,
            model TEXT,
            total_tokens INTEGER NOT NULL,
            error TEXT,
            actions_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS reflection_run_started
            ON reflection_runs(started_at DESC);
        ",
    )?;
    consolidate_duplicate_episodes(&mut connection)?;
    connection.execute(
        "
        CREATE UNIQUE INDEX IF NOT EXISTS episode_normalized_title
        ON episodes(lower(trim(title)))
        ",
        [],
    )?;
    connection.execute(
        "
        INSERT OR IGNORE INTO episode_messages (
            episode_id, revision_id, associated_at, association_source
        )
        SELECT episodes.id, json_each.value, episodes.updated_at, 'migration'
        FROM episodes, json_each(episodes.source_revision_ids_json)
        ",
        [],
    )?;
    connection.execute(
        "
        UPDATE reflection_runs SET
            status = 'interrupted',
            completed_at = COALESCE(completed_at, ?1),
            error = COALESCE(error, 'interrupted_by_service_restart')
        WHERE status = 'running'
        ",
        params![now()],
    )?;
    Ok(())
}

fn consolidate_duplicate_episodes(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let episodes = {
        let mut statement = transaction.prepare(
            "
            SELECT id, title, summary, state, started_at, last_activity_at,
                   updated_at, source_revision_ids_json, related_episode_ids_json
            FROM episodes
            ORDER BY id
            ",
        )?;
        statement
            .query_map([], episode_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut grouped = HashMap::<String, Vec<ConversationEpisode>>::new();
    for episode in episodes {
        grouped
            .entry(episode.title.trim().to_lowercase())
            .or_default()
            .push(episode);
    }

    let mut aliases = HashMap::<String, String>::new();
    for group in grouped.values_mut().filter(|group| group.len() > 1) {
        let mut counts = HashMap::<String, u64>::new();
        for episode in group.iter() {
            let count = transaction.query_row(
                "SELECT COUNT(*) FROM episode_messages WHERE episode_id = ?1",
                params![episode.id],
                |row| row.get::<_, i64>(0),
            )?;
            counts.insert(episode.id.clone(), count as u64);
        }
        group.sort_by(|left, right| {
            counts[&right.id]
                .cmp(&counts[&left.id])
                .then_with(|| left.started_at.cmp(&right.started_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        let canonical = group[0].clone();
        let duplicate_ids = group
            .iter()
            .skip(1)
            .map(|episode| episode.id.clone())
            .collect::<Vec<_>>();
        let mut sources = canonical.source_revision_ids.clone();
        let mut parents = canonical.parent_episode_ids.clone();
        for duplicate in group.iter().skip(1) {
            for source in &duplicate.source_revision_ids {
                if !sources.contains(source) && sources.len() < 50 {
                    sources.push(source.clone());
                }
            }
            for parent in &duplicate.parent_episode_ids {
                if !parents.contains(parent) {
                    parents.push(parent.clone());
                }
            }
            transaction.execute(
                "
                INSERT OR IGNORE INTO episode_messages (
                    episode_id, revision_id, associated_at, association_source
                )
                SELECT ?1, revision_id, associated_at, 'duplicate_consolidation'
                FROM episode_messages
                WHERE episode_id = ?2
                ",
                params![canonical.id, duplicate.id],
            )?;
            transaction.execute(
                "UPDATE episode_aliases SET canonical_id = ?1 WHERE canonical_id = ?2",
                params![canonical.id, duplicate.id],
            )?;
            transaction.execute(
                "
                INSERT INTO episode_aliases (alias_id, canonical_id, merged_at)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(alias_id) DO UPDATE SET
                    canonical_id = excluded.canonical_id,
                    merged_at = excluded.merged_at
                ",
                params![duplicate.id, canonical.id, now()],
            )?;
            aliases.insert(duplicate.id.clone(), canonical.id.clone());
        }
        parents.retain(|parent| parent != &canonical.id && !duplicate_ids.contains(parent));
        parents = dedup(parents);
        let started_at = group
            .iter()
            .map(|episode| episode.started_at.as_str())
            .min()
            .unwrap_or(canonical.started_at.as_str());
        let last_activity_at = group
            .iter()
            .map(|episode| episode.last_activity_at.as_str())
            .max()
            .unwrap_or(canonical.last_activity_at.as_str());
        transaction.execute(
            "
            UPDATE episodes SET
                started_at = ?2,
                last_activity_at = ?3,
                updated_at = ?4,
                source_revision_ids_json = ?5,
                related_episode_ids_json = ?6
            WHERE id = ?1
            ",
            params![
                canonical.id,
                started_at,
                last_activity_at,
                now(),
                serde_json::to_string(&sources)?,
                serde_json::to_string(&parents)?
            ],
        )?;
        for duplicate_id in duplicate_ids {
            transaction.execute(
                "DELETE FROM episode_messages WHERE episode_id = ?1",
                params![duplicate_id],
            )?;
            transaction.execute("DELETE FROM episodes WHERE id = ?1", params![duplicate_id])?;
        }
    }

    if !aliases.is_empty() {
        let remaining_parents = {
            let mut statement = transaction
                .prepare("SELECT id, related_episode_ids_json FROM episodes ORDER BY id")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (episode_id, encoded_parents) in remaining_parents {
            let original =
                serde_json::from_str::<Vec<String>>(&encoded_parents).unwrap_or_default();
            let mut resolved = Vec::with_capacity(original.len());
            for parent in &original {
                let parent = aliases
                    .get(parent)
                    .cloned()
                    .unwrap_or_else(|| parent.clone());
                if parent != episode_id {
                    resolved.push(parent);
                }
            }
            let resolved = dedup(resolved);
            if resolved != original {
                transaction.execute(
                    "UPDATE episodes SET related_episode_ids_json = ?2, updated_at = ?3 WHERE id = ?1",
                    params![episode_id, serde_json::to_string(&resolved)?, now()],
                )?;
            }
        }
    }
    transaction.commit()?;
    Ok(())
}

fn open_connection(path: &PathBuf) -> Result<Connection> {
    Connection::open(path).with_context(|| format!("open Reflection database {}", path.display()))
}

async fn read_config(path: &PathBuf) -> Result<ReflectionConfig> {
    match fs::read_to_string(path).await {
        Ok(content) => toml::from_str(&content)
            .with_context(|| format!("parse Reflection config {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ReflectionConfig::default()),
        Err(error) => {
            Err(error).with_context(|| format!("read Reflection config {}", path.display()))
        }
    }
}

fn validate_config(config: &ReflectionConfig) -> Result<()> {
    if !(5..=600).contains(&config.settle_seconds) {
        anyhow::bail!("Reflection settle time must be between 5 and 600 seconds");
    }
    if !(1..=3_650).contains(&config.retention_days) {
        anyhow::bail!("Reflection retention must be between 1 and 3650 days");
    }
    if config.daily_token_limit > MAX_DAILY_TOKEN_LIMIT {
        anyhow::bail!("Reflection daily token limit cannot exceed {MAX_DAILY_TOKEN_LIMIT}");
    }
    Ok(())
}

fn read_projection_health(connection: &Connection, now: DateTime<Utc>) -> Result<ProjectionHealth> {
    let episodes = read_episodes(connection, 1_000)?;
    let hypotheses = read_hypotheses(connection, 1_000)?;
    let last_lifecycle_review_at = connection
        .query_row(
            "SELECT value FROM reflection_meta WHERE key = 'last_lifecycle_review_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(lifecycle::projection_health(
        &episodes,
        &hypotheses,
        last_lifecycle_review_at,
        now,
    ))
}

async fn persist_file(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write Reflection config {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace Reflection config {}", path.display()))
}

fn reply_timing(
    connection: &Connection,
    related_revision_id: Option<&str>,
    user_at: &str,
) -> Result<Value> {
    let Some(related_revision_id) = related_revision_id else {
        return Ok(Value::Null);
    };
    let seen_at = connection
        .query_row(
            "
            SELECT occurred_at FROM conversation_events
            WHERE kind = 'message_seen' AND revision_id = ?1
            ORDER BY id DESC LIMIT 1
            ",
            params![related_revision_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let assistant_at = connection
        .query_row(
            "
            SELECT occurred_at FROM conversation_events
            WHERE kind = 'message_assistant' AND revision_id = ?1
            ORDER BY id DESC LIMIT 1
            ",
            params![related_revision_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let (basis, basis_at) = seen_at
        .map(|at| ("seen", at))
        .or_else(|| assistant_at.map(|at| ("published", at)))
        .unwrap_or(("unknown", String::new()));
    let delay_ms = duration_ms(&basis_at, user_at);
    Ok(json!({
        "basis": basis,
        "basisAt": if basis_at.is_empty() { Value::Null } else { json!(basis_at) },
        "delayMs": delay_ms
    }))
}

fn duration_ms(from: &str, to: &str) -> Option<i64> {
    let from = DateTime::parse_from_rfc3339(from).ok()?;
    let to = DateTime::parse_from_rfc3339(to).ok()?;
    Some((to - from).num_milliseconds().max(0))
}

fn read_cursor(connection: &Connection) -> Result<i64> {
    let value = connection.query_row(
        "SELECT value FROM reflection_meta WHERE key = 'cursor'",
        [],
        |row| row.get::<_, String>(0),
    )?;
    Ok(value.parse().unwrap_or_default())
}

fn read_events_after(
    connection: &Connection,
    cursor: i64,
    limit: usize,
) -> Result<Vec<InteractionEvent>> {
    let mut statement = connection.prepare(
        "
        SELECT id, kind, occurred_at, revision_id, related_revision_id,
               role, content_chars, payload_json, retracted
        FROM conversation_events
        WHERE id > ?1
          AND kind <> 'message_seen'
          AND retracted = 0
        ORDER BY id
        LIMIT ?2
        ",
    )?;
    statement
        .query_map(params![cursor, limit as i64], event_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn actionable_prefix(
    mut events: Vec<InteractionEvent>,
    preferred_limit: usize,
) -> Vec<InteractionEvent> {
    let mut saw_user = false;
    let mut boundaries = Vec::new();
    for (index, event) in events.iter().enumerate() {
        match event.kind.as_str() {
            "message_user" => saw_user = true,
            "message_assistant" if saw_user => {
                boundaries.push(index);
                saw_user = false;
            }
            "turn_settled" | "turn_reaction" if saw_user => {
                boundaries.push(index);
                saw_user = false;
            }
            "message_retracted" => boundaries.push(index),
            _ => {}
        }
    }
    let Some(boundary) = boundaries
        .iter()
        .copied()
        .take_while(|index| *index < preferred_limit)
        .last()
        .or_else(|| boundaries.first().copied())
    else {
        return Vec::new();
    };
    events.truncate(boundary + 1);
    events
}

fn read_recent_events(connection: &Connection, limit: usize) -> Result<Vec<InteractionEvent>> {
    let mut statement = connection.prepare(
        "
        SELECT id, kind, occurred_at, revision_id, related_revision_id,
               role, content_chars, payload_json, retracted
        FROM conversation_events
        WHERE retracted = 0
        ORDER BY id DESC
        LIMIT ?1
        ",
    )?;
    let mut events = statement
        .query_map(params![limit as i64], event_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    events.reverse();
    Ok(events)
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InteractionEvent> {
    let payload: String = row.get(7)?;
    Ok(InteractionEvent {
        id: row.get(0)?,
        kind: row.get(1)?,
        occurred_at: row.get(2)?,
        revision_id: row.get(3)?,
        related_revision_id: row.get(4)?,
        role: row.get(5)?,
        content_chars: row.get::<_, i64>(6)? as u64,
        payload: serde_json::from_str(&payload).unwrap_or_else(|_| json!({})),
        retracted: row.get::<_, i64>(8)? != 0,
    })
}

fn read_episodes(connection: &Connection, limit: usize) -> Result<Vec<ConversationEpisode>> {
    let mut statement = connection.prepare(
        "
        SELECT id, title, summary, state, started_at, last_activity_at,
               updated_at, source_revision_ids_json, related_episode_ids_json
        FROM episodes
        ORDER BY
            CASE state
                WHEN 'forming' THEN 0
                WHEN 'active' THEN 1
                WHEN 'dormant' THEN 2
                ELSE 3
            END,
            last_activity_at DESC
        LIMIT ?1
        ",
    )?;
    statement
        .query_map(params![limit as i64], episode_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn read_episode(connection: &Connection, id: &str) -> Result<Option<ConversationEpisode>> {
    connection
        .query_row(
            "
            SELECT id, title, summary, state, started_at, last_activity_at,
                   updated_at, source_revision_ids_json, related_episode_ids_json
            FROM episodes WHERE id = ?1
            ",
            params![id],
            episode_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn episode_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationEpisode> {
    let state: String = row.get(3)?;
    let sources: String = row.get(7)?;
    let related: String = row.get(8)?;
    Ok(ConversationEpisode {
        id: row.get(0)?,
        title: row.get(1)?,
        summary: row.get(2)?,
        state: EpisodeState::parse(&state).unwrap_or(EpisodeState::Dormant),
        started_at: row.get(4)?,
        last_activity_at: row.get(5)?,
        updated_at: row.get(6)?,
        source_revision_ids: serde_json::from_str(&sources).unwrap_or_default(),
        parent_episode_ids: serde_json::from_str(&related).unwrap_or_default(),
    })
}

fn read_hypotheses(connection: &Connection, limit: usize) -> Result<Vec<WorkingHypothesis>> {
    let mut statement = connection.prepare(
        "
        SELECT id, statement, evidence, alternatives, status, horizon,
               revisit_after, updated_at, source_revision_ids_json
        FROM hypotheses
        ORDER BY
            CASE status WHEN 'working' THEN 0 WHEN 'tentative' THEN 1 ELSE 2 END,
            updated_at DESC
        LIMIT ?1
        ",
    )?;
    statement
        .query_map(params![limit as i64], hypothesis_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn read_hypothesis(connection: &Connection, id: &str) -> Result<Option<WorkingHypothesis>> {
    connection
        .query_row(
            "
            SELECT id, statement, evidence, alternatives, status, horizon,
                   revisit_after, updated_at, source_revision_ids_json
            FROM hypotheses WHERE id = ?1
            ",
            params![id],
            hypothesis_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn hypothesis_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkingHypothesis> {
    let status: String = row.get(4)?;
    let horizon: String = row.get(5)?;
    let sources: String = row.get(8)?;
    Ok(WorkingHypothesis {
        id: row.get(0)?,
        statement: row.get(1)?,
        evidence: row.get(2)?,
        alternatives: row.get(3)?,
        status: HypothesisStatus::parse(&status).unwrap_or(HypothesisStatus::Tentative),
        horizon: HypothesisHorizon::parse(&horizon).unwrap_or(HypothesisHorizon::Current),
        revisit_after: row.get(6)?,
        updated_at: row.get(7)?,
        source_revision_ids: serde_json::from_str(&sources).unwrap_or_default(),
    })
}

fn read_follow_ups(connection: &Connection, limit: usize) -> Result<Vec<DeferredFollowUp>> {
    let mut statement = connection.prepare(
        "
        SELECT id, reason, not_before, status, created_at, updated_at,
               triggered_at, completed_at, outcome, source_revision_ids_json
        FROM follow_ups
        ORDER BY
            CASE status WHEN 'triggered' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
            not_before DESC
        LIMIT ?1
        ",
    )?;
    statement
        .query_map(params![limit as i64], follow_up_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn read_follow_up(connection: &Connection, id: &str) -> Result<Option<DeferredFollowUp>> {
    connection
        .query_row(
            "
            SELECT id, reason, not_before, status, created_at, updated_at,
                   triggered_at, completed_at, outcome, source_revision_ids_json
            FROM follow_ups WHERE id = ?1
            ",
            params![id],
            follow_up_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn follow_up_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeferredFollowUp> {
    let sources: String = row.get(9)?;
    Ok(DeferredFollowUp {
        id: row.get(0)?,
        reason: row.get(1)?,
        not_before: row.get(2)?,
        status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        triggered_at: row.get(6)?,
        completed_at: row.get(7)?,
        outcome: row.get(8)?,
        source_revision_ids: serde_json::from_str(&sources).unwrap_or_default(),
    })
}

fn read_runs(connection: &Connection, limit: usize) -> Result<Vec<ReflectionRun>> {
    let mut statement = connection.prepare(
        "
        SELECT id, trigger, status, started_at, completed_at, from_event_id,
               to_event_id, event_count, summary, trace_id, model,
               total_tokens, error, actions_json
        FROM reflection_runs
        ORDER BY started_at DESC
        LIMIT ?1
        ",
    )?;
    statement
        .query_map(params![limit.clamp(1, 50) as i64], |row| {
            let actions: String = row.get(13)?;
            Ok(ReflectionRun {
                id: row.get(0)?,
                trigger: row.get(1)?,
                status: row.get(2)?,
                started_at: row.get(3)?,
                completed_at: row.get(4)?,
                from_event_id: row.get(5)?,
                to_event_id: row.get(6)?,
                event_count: row.get::<_, i64>(7)? as u64,
                summary: row.get(8)?,
                trace_id: row.get(9)?,
                model: row.get(10)?,
                total_tokens: row.get::<_, i64>(11)? as u64,
                error: row.get(12)?,
                actions: serde_json::from_str(&actions).unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn format_source_bundle(
    events: &[InteractionEvent],
    episodes: &[ConversationEpisode],
    hypotheses: &[WorkingHypothesis],
    follow_ups: &[DeferredFollowUp],
) -> String {
    let mut output = vec![format!(
        "<time now=\"{}\" timezone=\"{}\" />",
        Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        Local::now().format("%Z")
    )];
    output.push("<new-interaction-events>".to_owned());
    for event in events {
        output.push(format_event(event));
    }
    output.push("</new-interaction-events>".to_owned());
    append_state_sections(&mut output, episodes, hypotheses, follow_ups);
    truncate(&output.join("\n"), MAX_PROMPT_CHARS)
}

fn format_temporal_prompt(
    events: &[InteractionEvent],
    episodes: &[ConversationEpisode],
    hypotheses: &[WorkingHypothesis],
    follow_ups: &[DeferredFollowUp],
) -> String {
    let now = Local::now();
    let mut output = vec![
        "<symbiont-temporal-context>".to_owned(),
        format!(
            "<now value=\"{}\" timezone=\"{}\" />",
            now.to_rfc3339_opts(SecondsFormat::Secs, false),
            now.format("%Z")
        ),
    ];
    if !events.is_empty() {
        output.push("<recent-interaction-shape>".to_owned());
        for event in events {
            output.push(format_event(event));
        }
        output.push("</recent-interaction-shape>".to_owned());
    }
    append_state_sections(&mut output, episodes, hypotheses, follow_ups);
    output.push(
        "Facts and model interpretations are separate. Timing is contextual evidence, never a rating."
            .to_owned(),
    );
    output.push("</symbiont-temporal-context>".to_owned());
    output.join("\n")
}

fn append_state_sections(
    output: &mut Vec<String>,
    episodes: &[ConversationEpisode],
    hypotheses: &[WorkingHypothesis],
    follow_ups: &[DeferredFollowUp],
) {
    if !episodes.is_empty() {
        output.push("<episodes>".to_owned());
        for episode in episodes {
            output.push(format!(
                "<episode id=\"{}\" state=\"{}\" last_activity=\"{}\" sources=\"{}\" parents=\"{}\">\n# {}\n{}\n</episode>",
                episode.id,
                episode.state.as_str(),
                episode.last_activity_at,
                episode.source_revision_ids.join(","),
                episode.parent_episode_ids.join(","),
                episode.title,
                episode.summary
            ));
        }
        output.push("</episodes>".to_owned());
    }
    let active_hypotheses = hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.status.is_active())
        .collect::<Vec<_>>();
    if !active_hypotheses.is_empty() {
        output.push("<working-hypotheses>".to_owned());
        for hypothesis in active_hypotheses {
            output.push(format!(
                "<hypothesis id=\"{}\" status=\"{}\" horizon=\"{}\" revisit_after=\"{}\" sources=\"{}\">\nStatement: {}\nEvidence: {}\nAlternatives: {}\n</hypothesis>",
                hypothesis.id,
                hypothesis.status.as_str(),
                hypothesis.horizon.as_str(),
                hypothesis.revisit_after.as_deref().unwrap_or(""),
                hypothesis.source_revision_ids.join(","),
                hypothesis.statement,
                hypothesis.evidence,
                hypothesis.alternatives
            ));
        }
        output.push("</working-hypotheses>".to_owned());
    }
    let open_follow_ups = follow_ups
        .iter()
        .filter(|follow_up| matches!(follow_up.status.as_str(), "pending" | "triggered"))
        .collect::<Vec<_>>();
    if !open_follow_ups.is_empty() {
        output.push("<deferred-follow-ups>".to_owned());
        for follow_up in open_follow_ups {
            output.push(format!(
                "<follow-up id=\"{}\" status=\"{}\" not_before=\"{}\" sources=\"{}\">{}</follow-up>",
                follow_up.id,
                follow_up.status,
                follow_up.not_before,
                follow_up.source_revision_ids.join(","),
                follow_up.reason
            ));
        }
        output.push("</deferred-follow-ups>".to_owned());
    }
}

fn format_event(event: &InteractionEvent) -> String {
    let excerpt = event
        .payload
        .get("excerpt")
        .and_then(Value::as_str)
        .map(|value| truncate(value, 700))
        .unwrap_or_default();
    let timing = event
        .payload
        .get("replyTiming")
        .cloned()
        .unwrap_or(Value::Null);
    let hunch_feedback = event
        .payload
        .get("hunchFeedback")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(|target| {
                    Some(format!(
                        "{}@{}",
                        target.get("pageId")?.as_str()?,
                        target.get("revisionId")?.as_str()?
                    ))
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let reaction = event
        .payload
        .get("reaction")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!(
        "<event id=\"{}\" kind=\"{}\" at=\"{}\" revision=\"{}\" related=\"{}\" role=\"{}\" chars=\"{}\" retracted=\"{}\" reply_timing='{}' hunch_feedback=\"{}\" reaction=\"{}\">{}</event>",
        event.id,
        event.kind,
        event.occurred_at,
        event.revision_id.as_deref().unwrap_or(""),
        event.related_revision_id.as_deref().unwrap_or(""),
        event.role.as_deref().unwrap_or(""),
        event.content_chars,
        event.retracted,
        timing,
        hunch_feedback,
        reaction,
        excerpt
    )
}

fn source_time(connection: &Connection, revision_ids: &[String], latest: bool) -> Option<String> {
    let mut values = revision_ids
        .iter()
        .filter_map(|revision_id| {
            connection
                .query_row(
                    "
                    SELECT occurred_at FROM conversation_events
                    WHERE revision_id = ?1 AND kind LIKE 'message_%'
                    ORDER BY id DESC LIMIT 1
                    ",
                    params![revision_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
        })
        .collect::<Vec<_>>();
    values.sort();
    if latest {
        values.pop()
    } else {
        values.into_iter().next()
    }
}

fn read_episode_revision_ids(
    connection: &Connection,
    id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "
        SELECT em.revision_id
        FROM episode_messages em
        LEFT JOIN conversation_events ce ON ce.revision_id = em.revision_id
        WHERE em.episode_id = ?1 AND COALESCE(ce.retracted, 0) = 0
        GROUP BY em.revision_id
        ORDER BY MIN(COALESCE(ce.occurred_at, em.associated_at)) ASC
        LIMIT ?2
        ",
    )?;
    Ok(statement
        .query_map(params![id, limit.clamp(1, 200) as i64], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn validate_sources(sources: &[String]) -> Result<()> {
    if sources.is_empty() {
        anyhow::bail!("Reflection state requires exact source Revision IDs");
    }
    if sources.len() > 50 {
        anyhow::bail!("Reflection state accepts at most 50 source Revisions");
    }
    Ok(())
}

fn ensure_known_source_revisions(connection: &Connection, sources: &[String]) -> Result<()> {
    for revision_id in sources {
        if !is_known_source_revision(connection, revision_id)? {
            anyhow::bail!("unknown conversation Revision: {revision_id}");
        }
    }
    Ok(())
}

fn ensure_episode_exists(connection: &Connection, id: &str) -> Result<()> {
    let exists = connection
        .query_row("SELECT 1 FROM episodes WHERE id = ?1", params![id], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if !exists {
        anyhow::bail!(
            "unknown Episode ID `{id}`; use an exact ID from Reflection state or omit episode_id to create a genuinely new Episode"
        );
    }
    Ok(())
}

fn find_episode_id_by_title(connection: &Connection, title: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT id FROM episodes WHERE lower(trim(title)) = lower(trim(?1)) LIMIT 1",
            params![title],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
}

fn ensure_episode_title_is_available(
    connection: &Connection,
    title: &str,
    episode_id: &str,
) -> Result<()> {
    if let Some(existing_id) = find_episode_id_by_title(connection, title)?
        && existing_id != episode_id
    {
        anyhow::bail!(
            "Episode title already belongs to `{existing_id}`; revise or consolidate that exact Episode instead of renaming `{episode_id}` over it"
        );
    }
    Ok(())
}

fn resolve_episode_alias(connection: &Connection, id: &str) -> Result<String> {
    let mut current = id.to_owned();
    for _ in 0..16 {
        let next = connection
            .query_row(
                "SELECT canonical_id FROM episode_aliases WHERE alias_id = ?1",
                params![current],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(next) = next else {
            return Ok(current);
        };
        if next == current {
            anyhow::bail!("Episode alias cycle at `{current}`");
        }
        current = next;
    }
    anyhow::bail!("Episode alias chain is too deep for `{id}`")
}

fn expand_conversation_turns(
    connection: &Connection,
    revision_ids: &[String],
) -> Result<Vec<String>> {
    let mut expanded = dedup(revision_ids.to_vec());
    let requested = expanded.clone();
    for revision_id in requested {
        let event = connection
            .query_row(
                "
                SELECT role, related_revision_id
                FROM conversation_events
                WHERE revision_id = ?1
                  AND kind IN ('message_user', 'message_assistant')
                  AND retracted = 0
                ORDER BY id DESC
                LIMIT 1
                ",
                params![revision_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        let Some((role, related_revision_id)) = event else {
            continue;
        };
        match role.as_deref() {
            Some("assistant") => {
                if let Some(user_revision_id) = related_revision_id {
                    expanded.push(user_revision_id);
                }
            }
            Some("user") => {
                let mut replies = connection.prepare(
                    "
                    SELECT revision_id
                    FROM conversation_events
                    WHERE kind = 'message_assistant'
                      AND related_revision_id = ?1
                      AND revision_id IS NOT NULL
                      AND retracted = 0
                    ORDER BY id
                    ",
                )?;
                expanded.extend(
                    replies
                        .query_map(params![revision_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            _ => {}
        }
    }
    Ok(dedup(expanded))
}

fn insert_episode_messages(
    transaction: &Transaction<'_>,
    episode_id: &str,
    revision_ids: &[String],
    associated_at: &str,
    association_source: &str,
) -> Result<()> {
    for revision_id in revision_ids {
        transaction.execute(
            "
            INSERT OR IGNORE INTO episode_messages (
                episode_id, revision_id, associated_at, association_source
            ) VALUES (?1, ?2, ?3, ?4)
            ",
            params![episode_id, revision_id, associated_at, association_source],
        )?;
    }
    Ok(())
}

fn ensure_hypothesis_exists(connection: &Connection, id: &str) -> Result<()> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM hypotheses WHERE id = ?1",
            params![id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        anyhow::bail!(
            "unknown hypothesis ID `{id}`; use an exact ID from Reflection state or omit hypothesis_id for a distinct interpretation"
        );
    }
    Ok(())
}

fn ensure_hypothesis_statement_is_new(connection: &Connection, statement: &str) -> Result<()> {
    let existing = connection
        .query_row(
            "SELECT id FROM hypotheses WHERE lower(trim(statement)) = lower(trim(?1)) LIMIT 1",
            params![statement],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(id) = existing {
        anyhow::bail!(
            "hypothesis statement already exists as `{id}`; revise that exact hypothesis instead of creating a duplicate"
        );
    }
    Ok(())
}

fn is_known_source_revision(connection: &Connection, revision_id: &str) -> Result<bool> {
    connection
        .query_row(
            "
            SELECT 1
            WHERE EXISTS (
                SELECT 1 FROM conversation_events
                WHERE revision_id = ?1
                  AND kind IN ('message_user', 'message_assistant')
            )
            OR EXISTS (
                SELECT 1 FROM verified_revisions
                WHERE revision_id = ?1
            )
            ",
            params![revision_id],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(Into::into)
}

fn validate_episode_parents(connection: &Connection, id: &str, parents: &[String]) -> Result<()> {
    if parents.iter().any(|parent| parent == id) {
        anyhow::bail!("an Episode cannot be its own parent");
    }
    let episodes = read_episodes(connection, 10_000)?;
    let graph = episodes
        .iter()
        .map(|episode| (episode.id.as_str(), episode.parent_episode_ids.as_slice()))
        .collect::<std::collections::HashMap<_, _>>();
    for parent in parents {
        if !graph.contains_key(parent.as_str()) {
            anyhow::bail!("unknown parent Episode: {parent}");
        }
        if episode_reaches(&graph, parent, id, &mut std::collections::HashSet::new()) {
            anyhow::bail!("Episode parent relation would create a cycle");
        }
    }
    Ok(())
}

fn episode_reaches(
    graph: &std::collections::HashMap<&str, &[String]>,
    current: &str,
    target: &str,
    visited: &mut std::collections::HashSet<String>,
) -> bool {
    if current == target {
        return true;
    }
    if !visited.insert(current.to_owned()) {
        return false;
    }
    graph
        .get(current)
        .into_iter()
        .flat_map(|parents| parents.iter())
        .any(|parent| episode_reaches(graph, parent, target, visited))
}

fn dedup(mut values: Vec<String>) -> Vec<String> {
    values.retain(|value| !value.trim().is_empty());
    values.sort();
    values.dedup();
    values
}

fn new_id(prefix: &str) -> String {
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let material = format!("{}:{}:{}", now(), std::process::id(), counter);
    let digest = Sha256::digest(material.as_bytes());
    format!("{prefix}_{}", &format!("{digest:x}")[..24])
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output = value.chars().take(max_chars).collect::<String>();
    output.push_str("\n[truncated]");
    output
}
