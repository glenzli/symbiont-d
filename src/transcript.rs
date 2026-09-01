//! Authoritative local conversation transcript.
//!
//! PCP receives selected, durable source material for later recall.  It is not
//! the chat application's transcript store: a user must be able to browse,
//! edit, retract, and eventually expire a conversation even while PCP is
//! unavailable or its representation changes.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use getrandom::fill;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{sync::Mutex, task};

use crate::memory::{MemoryEntry, MemoryRole, MessagePart};

mod search;
mod semantic;

#[cfg(test)]
pub use search::TranscriptRecurrenceEvidence;
pub use search::{
    TranscriptRecall, TranscriptSearchMessage, TranscriptSearchOptions, TranscriptSearchResult,
    TranscriptSemanticEvidence, TranscriptSourceOptions, TranscriptSourceResolution,
    TranscriptSourceStatus,
};

const SCHEMA_VERSION: &str = "5";

#[derive(Clone, Debug, Default)]
pub struct TranscriptRestoreReport {
    pub imported_messages: usize,
    pub source_snapshot: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct TranscriptMessage {
    pub entry: MemoryEntry,
    pub message_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptMessageLinks {
    pub responds_to: Option<String>,
    pub continues_from: Option<String>,
    #[serde(default)]
    pub input_revision_ids: Vec<String>,
    #[serde(default)]
    pub surfaced_hunch_revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TranscriptRetraction {
    pub retracted_message_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationTranscriptMessage {
    pub sequence: i64,
    pub message_id: String,
    pub entry: MemoryEntry,
}

/// The connection is intentionally opened per operation.  It keeps all
/// blocking SQLite work off the async runtime and makes process restart a
/// normal part of the transcript contract.
#[derive(Clone)]
pub struct TranscriptStore {
    path: PathBuf,
    mutation: Arc<Mutex<()>>,
    source_store_id: String,
}

impl TranscriptStore {
    pub async fn open(
        path: PathBuf,
        legacy_snapshot: Option<PathBuf>,
    ) -> Result<(Self, TranscriptRestoreReport)> {
        let mut store = Self {
            path,
            mutation: Arc::new(Mutex::new(())),
            source_store_id: String::new(),
        };
        store.initialize().await?;
        store.source_store_id = store.ensure_source_store_id().await?;
        let report = store.restore_if_empty(legacy_snapshot).await?;
        Ok((store, report))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source_store_id(&self) -> &str {
        &self.source_store_id
    }

    async fn ensure_source_store_id(&self) -> Result<String> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<String> {
            let connection = open_connection(&path)?;
            if let Some(source_store_id) = connection
                .query_row(
                    "SELECT value FROM transcript_meta WHERE key = 'source_store_id'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                return Ok(source_store_id);
            }
            let source_store_id = new_source_store_id();
            connection.execute(
                "INSERT INTO transcript_meta(key, value) VALUES ('source_store_id', ?1)",
                [&source_store_id],
            )?;
            Ok(source_store_id)
        })
        .await
        .context("join transcript source store identity initialization")?
    }

    async fn initialize(&self) -> Result<()> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create transcript directory {}", parent.display()))?;
            }
            let connection = open_connection(&path)?;
            connection.execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS transcript_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS transcript_messages (
                    message_id TEXT PRIMARY KEY,
                    sequence INTEGER NOT NULL UNIQUE,
                    occurred_at TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    entry_json TEXT NOT NULL,
                    imported_source_revision_id TEXT UNIQUE,
                    retracted_at TEXT
                );
                CREATE INDEX IF NOT EXISTS transcript_messages_visible
                    ON transcript_messages(retracted_at, sequence DESC);
                CREATE INDEX IF NOT EXISTS transcript_messages_at
                    ON transcript_messages(occurred_at, sequence DESC);
                CREATE TABLE IF NOT EXISTS transcript_imports (
                    source_snapshot TEXT NOT NULL,
                    source_revision_id TEXT NOT NULL UNIQUE,
                    imported_at TEXT NOT NULL,
                    PRIMARY KEY(source_snapshot, source_revision_id)
                );
                CREATE TABLE IF NOT EXISTS transcript_message_links (
                    message_id TEXT PRIMARY KEY,
                    links_json TEXT NOT NULL,
                    FOREIGN KEY(message_id) REFERENCES transcript_messages(message_id) ON DELETE CASCADE
                );
                CREATE TABLE IF NOT EXISTS transcript_pcp_migration_watermarks (
                    pcp_identity_id TEXT PRIMARY KEY,
                    completed_sequence INTEGER NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS transcript_message_embeddings (
                    message_id TEXT NOT NULL,
                    embedding_space TEXT NOT NULL,
                    dimensions INTEGER NOT NULL,
                    normalized INTEGER NOT NULL,
                    distance_metric TEXT NOT NULL,
                    vector_blob BLOB NOT NULL,
                    indexed_at TEXT NOT NULL,
                    PRIMARY KEY(message_id, embedding_space),
                    FOREIGN KEY(message_id) REFERENCES transcript_messages(message_id)
                        ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS transcript_embeddings_space
                    ON transcript_message_embeddings(embedding_space, message_id);
                ",
            )?;
            search::initialize_index(&connection)?;
            connection.execute(
                "INSERT OR REPLACE INTO transcript_meta(key, value) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION],
            )?;
            Ok(())
        })
        .await
        .context("join transcript initialization")??;
        Ok(())
    }

    async fn restore_if_empty(
        &self,
        legacy_snapshot: Option<PathBuf>,
    ) -> Result<TranscriptRestoreReport> {
        let Some(snapshot) = legacy_snapshot.filter(|path| path.is_file()) else {
            return Ok(TranscriptRestoreReport::default());
        };
        let _guard = self.mutation.lock().await;
        let destination = self.path.clone();
        let source = snapshot.clone();
        task::spawn_blocking(move || restore_if_empty_blocking(&destination, &source))
            .await
            .context("join transcript archive restore")?
    }

    pub async fn append(
        &self,
        mut entry: MemoryEntry,
        links: TranscriptMessageLinks,
    ) -> Result<TranscriptMessage> {
        let _guard = self.mutation.lock().await;
        let message_id = entry.revision_id.clone().unwrap_or_else(new_message_id);
        entry.revision_id = Some(message_id.clone());
        let path = self.path.clone();
        let stored = entry.clone();
        let persisted_message_id = message_id.clone();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            let sequence = next_sequence(&transaction)?;
            transaction.execute(
                "INSERT INTO transcript_messages(
                    message_id, sequence, occurred_at, role, content, entry_json,
                    imported_source_revision_id, retracted_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
                params![
                    persisted_message_id,
                    sequence,
                    stored.at,
                    role_name(&stored.role),
                    stored.content,
                    serde_json::to_string(&stored)?,
                ],
            )?;
            transaction.execute(
                "INSERT INTO transcript_message_links(message_id, links_json) VALUES (?1, ?2)",
                params![persisted_message_id, serde_json::to_string(&links)?],
            )?;
            search::index_message(&transaction, &persisted_message_id, &stored.content)?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join transcript append")??;
        Ok(TranscriptMessage { entry, message_id })
    }

    pub async fn recent(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let path = self.path.clone();
        task::spawn_blocking(move || read_recent(&path, limit, None))
            .await
            .context("join transcript recent read")?
    }

    pub async fn before(
        &self,
        before_at: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<MemoryEntry>, bool)> {
        let path = self.path.clone();
        let before = before_at.map(str::to_owned);
        task::spawn_blocking(move || read_history_page(&path, before.as_deref(), limit))
            .await
            .context("join transcript history read")?
    }

    pub async fn by_ids(&self, message_ids: &[String]) -> Result<Vec<MemoryEntry>> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let path = self.path.clone();
        let ids = message_ids.to_vec();
        task::spawn_blocking(move || read_by_ids(&path, &ids))
            .await
            .context("join transcript selected read")?
    }

    /// Search the authoritative local transcript without copying chat into PCP.
    ///
    /// The result contains bounded raw-message windows plus user-only recurrence
    /// evidence suitable for deciding whether a previously deferred subject now
    /// deserves durable promotion.
    pub async fn search(
        &self,
        query: &str,
        options: TranscriptSearchOptions,
    ) -> Result<TranscriptSearchResult> {
        self.search_with_semantic(query, options, Vec::new(), None)
            .await
    }

    async fn search_with_semantic(
        &self,
        query: &str,
        options: TranscriptSearchOptions,
        semantic_matches: Vec<semantic::SemanticMatch>,
        semantic_evidence: Option<search::TranscriptSemanticEvidence>,
    ) -> Result<TranscriptSearchResult> {
        let path = self.path.clone();
        let query = query.to_owned();
        task::spawn_blocking(move || {
            search::search_transcript(&path, &query, options, &semantic_matches, semantic_evidence)
        })
        .await
        .context("join transcript search")?
    }

    pub async fn resolve_source(
        &self,
        message_id: &str,
        options: TranscriptSourceOptions,
    ) -> Result<TranscriptSourceResolution> {
        let path = self.path.clone();
        let message_id = message_id.to_owned();
        task::spawn_blocking(move || search::resolve_source(&path, &message_id, options))
            .await
            .context("join transcript source resolution")?
    }

    pub async fn links(&self, message_id: &str) -> Result<TranscriptMessageLinks> {
        let path = self.path.clone();
        let message_id = message_id.to_owned();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            let links = connection
                .query_row(
                    "SELECT links_json FROM transcript_message_links WHERE message_id = ?1",
                    [message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            links
                .map(|value| serde_json::from_str(&value).context("decode transcript links"))
                .unwrap_or_else(|| Ok(TranscriptMessageLinks::default()))
        })
        .await
        .context("join transcript link read")?
    }

    pub async fn max_visible_sequence(&self) -> Result<i64> {
        let path = self.path.clone();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            connection
                .query_row(
                    "SELECT COALESCE(MAX(sequence), 0) FROM transcript_messages WHERE retracted_at IS NULL",
                    [],
                    |row| row.get(0),
                )
                .context("read transcript migration upper bound")
        })
        .await
        .context("join transcript migration upper-bound read")?
    }

    pub async fn pcp_migration_watermark(&self, pcp_identity_id: &str) -> Result<i64> {
        let path = self.path.clone();
        let pcp_identity_id = pcp_identity_id.to_owned();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            connection
                .query_row(
                    "SELECT completed_sequence FROM transcript_pcp_migration_watermarks WHERE pcp_identity_id = ?1",
                    [pcp_identity_id],
                    |row| row.get(0),
                )
                .optional()
                .map(|value| value.unwrap_or_default())
                .context("read PCP transcript migration watermark")
        })
        .await
        .context("join PCP transcript migration watermark read")?
    }

    pub async fn pcp_migration_batch(
        &self,
        after_sequence: i64,
        through_sequence: i64,
        limit: usize,
    ) -> Result<Vec<MigrationTranscriptMessage>> {
        let path = self.path.clone();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            let mut statement = connection.prepare(
                "SELECT sequence, message_id, entry_json
                 FROM transcript_messages
                 WHERE retracted_at IS NULL AND sequence > ?1 AND sequence <= ?2
                 ORDER BY sequence
                 LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![after_sequence, through_sequence, limit.clamp(1, 100) as i64],
                |row| {
                    let entry_json = row.get::<_, String>(2)?;
                    Ok(MigrationTranscriptMessage {
                        sequence: row.get(0)?,
                        message_id: row.get(1)?,
                        entry: serde_json::from_str(&entry_json).map_err(to_sql_error)?,
                    })
                },
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("read PCP transcript migration batch")
        })
        .await
        .context("join PCP transcript migration batch read")?
    }

    pub async fn advance_pcp_migration_watermark(
        &self,
        pcp_identity_id: &str,
        completed_sequence: i64,
    ) -> Result<()> {
        let _guard = self.mutation.lock().await;
        let path = self.path.clone();
        let pcp_identity_id = pcp_identity_id.to_owned();
        task::spawn_blocking(move || {
            let connection = open_connection(&path)?;
            connection.execute(
                "INSERT INTO transcript_pcp_migration_watermarks(
                    pcp_identity_id, completed_sequence, updated_at
                 ) VALUES(?1, ?2, ?3)
                 ON CONFLICT(pcp_identity_id) DO UPDATE SET
                    completed_sequence=MAX(completed_sequence, excluded.completed_sequence),
                    updated_at=excluded.updated_at",
                params![pcp_identity_id, completed_sequence, now()],
            )?;
            Ok(())
        })
        .await
        .context("join PCP transcript migration watermark write")?
    }

    pub async fn retract_from(&self, message_id: &str) -> Result<TranscriptRetraction> {
        let _guard = self.mutation.lock().await;
        let path = self.path.clone();
        let message_id = message_id.to_owned();
        task::spawn_blocking(move || retract_from_blocking(&path, &message_id))
            .await
            .context("join transcript retraction")?
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("open transcript database {}", path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .context("configure transcript SQLite busy timeout")?;
    Ok(connection)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn to_sql_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn next_sequence(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM transcript_messages",
        [],
        |row| row.get(0),
    )?)
}

fn role_name(role: &MemoryRole) -> &'static str {
    match role {
        MemoryRole::User => "user",
        MemoryRole::Assistant => "assistant",
        MemoryRole::Memory => "memory",
    }
}

fn new_message_id() -> String {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes).expect("operating-system randomness for local message id");
    format!(
        "msg_{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn new_source_store_id() -> String {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes).expect("operating-system randomness for transcript source store id");
    format!(
        "src_{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn restore_if_empty_blocking(destination: &Path, source: &Path) -> Result<TranscriptRestoreReport> {
    let mut destination = open_connection(destination)?;
    let existing: i64 =
        destination.query_row("SELECT COUNT(*) FROM transcript_messages", [], |row| {
            row.get(0)
        })?;
    if existing > 0 {
        return Ok(TranscriptRestoreReport::default());
    }

    let source_connection = Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open archived PCP snapshot {}", source.display()))?;
    let mut query = source_connection.prepare(
        "SELECT r.revision_id, COALESCE(r.observed_at, r.created_at), r.payload_content, r.facets_json
         FROM pcp_pages p
         JOIN pcp_revisions r ON r.revision_id = p.current_revision_id
         WHERE p.kind = 'conversation_event'
         ORDER BY COALESCE(r.observed_at, r.created_at), r.revision_id",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let imported_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let transaction = destination.transaction()?;
    let mut sequence = 0_i64;
    let mut imported = 0_usize;
    for row in rows {
        let (source_revision_id, occurred_at, payload, facets) = row?;
        let Some(entry) = archived_entry(&source_revision_id, &occurred_at, payload, facets) else {
            continue;
        };
        sequence += 1;
        transaction.execute(
            "INSERT OR IGNORE INTO transcript_messages(
                message_id, sequence, occurred_at, role, content, entry_json,
                imported_source_revision_id, retracted_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                source_revision_id,
                sequence,
                entry.at,
                role_name(&entry.role),
                entry.content,
                serde_json::to_string(&entry)?,
                source_revision_id,
            ],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO transcript_imports(source_snapshot, source_revision_id, imported_at)
             VALUES (?1, ?2, ?3)",
            params![source.display().to_string(), source_revision_id, imported_at],
        )?;
        search::index_message(&transaction, &source_revision_id, &entry.content)?;
        imported += 1;
    }
    transaction.commit()?;
    Ok(TranscriptRestoreReport {
        imported_messages: imported,
        source_snapshot: Some(source.to_owned()),
    })
}

fn archived_entry(
    revision_id: &str,
    occurred_at: &str,
    payload: Option<String>,
    facets: Option<String>,
) -> Option<MemoryEntry> {
    let facets = facets.and_then(|facets| serde_json::from_str::<Value>(&facets).ok())?;
    let role = match facets.get("role").and_then(Value::as_str)? {
        "user" => MemoryRole::User,
        "assistant" => MemoryRole::Assistant,
        "memory" => MemoryRole::Memory,
        _ => return None,
    };
    let content = payload.unwrap_or_default();
    let mut parts = facets
        .get("contentParts")
        .and_then(|value| serde_json::from_value::<Vec<MessagePart>>(value.clone()).ok())
        .unwrap_or_default();
    if parts.is_empty() && !content.is_empty() {
        parts.push(MessagePart::Markdown {
            text: content.clone(),
        });
    }
    Some(MemoryEntry {
        role,
        at: occurred_at.to_owned(),
        content,
        revision_id: Some(revision_id.to_owned()),
        parts,
        metadata: facets
            .get("messageMetadata")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        delivery_state: None,
    })
}

fn read_recent(path: &Path, limit: usize, before_at: Option<&str>) -> Result<Vec<MemoryEntry>> {
    let connection = open_connection(path)?;
    let mut entries: Vec<MemoryEntry> = Vec::new();
    let query = if before_at.is_some() {
        "SELECT entry_json FROM transcript_messages
         WHERE retracted_at IS NULL AND occurred_at < ?1
         ORDER BY sequence DESC LIMIT ?2"
    } else {
        "SELECT entry_json FROM transcript_messages
         WHERE retracted_at IS NULL
         ORDER BY sequence DESC LIMIT ?1"
    };
    let mut statement = connection.prepare(query)?;
    let mut rows = if let Some(before_at) = before_at {
        statement.query(params![before_at, limit.clamp(1, 500) as i64])?
    } else {
        statement.query(params![limit.clamp(1, 500) as i64])?
    };
    while let Some(row) = rows.next()? {
        let json: String = row.get(0)?;
        entries.push(serde_json::from_str(&json)?);
    }
    entries.reverse();
    Ok(entries)
}

fn read_history_page(
    path: &Path,
    before_at: Option<&str>,
    limit: usize,
) -> Result<(Vec<MemoryEntry>, bool)> {
    let requested = limit.clamp(1, 100);
    let mut entries = read_recent(path, requested + 1, before_at)?;
    let has_more = entries.len() > requested;
    if has_more {
        entries.remove(0);
    }
    Ok((entries, has_more))
}

fn read_by_ids(path: &Path, message_ids: &[String]) -> Result<Vec<MemoryEntry>> {
    let connection = open_connection(path)?;
    let mut statement = connection.prepare(
        "SELECT entry_json FROM transcript_messages WHERE message_id = ?1 AND retracted_at IS NULL",
    )?;
    let mut entries: Vec<MemoryEntry> = Vec::new();
    for message_id in message_ids {
        if let Some(json) = statement
            .query_row([message_id], |row| row.get::<_, String>(0))
            .optional()?
        {
            entries.push(serde_json::from_str(&json)?);
        }
    }
    entries.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.revision_id.cmp(&right.revision_id))
    });
    Ok(entries)
}

fn retract_from_blocking(path: &Path, message_id: &str) -> Result<TranscriptRetraction> {
    let mut connection = open_connection(path)?;
    let transaction = connection.transaction()?;
    let start = transaction
        .query_row(
            "SELECT sequence FROM transcript_messages WHERE message_id = ?1 AND retracted_at IS NULL",
            [message_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(start) = start else {
        return Ok(TranscriptRetraction::default());
    };
    let mut statement = transaction.prepare(
        "SELECT message_id FROM transcript_messages WHERE sequence >= ?1 AND retracted_at IS NULL ORDER BY sequence",
    )?;
    let ids = statement
        .query_map([start], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    transaction.execute(
        "UPDATE transcript_messages SET retracted_at = ?1 WHERE sequence >= ?2 AND retracted_at IS NULL",
        params![Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true), start],
    )?;
    transaction.commit()?;
    Ok(TranscriptRetraction {
        retracted_message_ids: ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: MemoryRole, at: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            role,
            at: at.to_owned(),
            content: content.to_owned(),
            revision_id: None,
            parts: vec![MessagePart::Markdown {
                text: content.to_owned(),
            }],
            metadata: None,
            delivery_state: None,
        }
    }

    #[tokio::test]
    async fn stores_chat_independently_of_pcp() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, report) =
            TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
                .await
                .expect("open transcript");
        assert_eq!(report.imported_messages, 0);
        let written = store
            .append(
                entry(MemoryRole::User, "2026-08-15T00:00:00Z", "local only"),
                TranscriptMessageLinks::default(),
            )
            .await
            .expect("append transcript message");
        assert!(written.message_id.starts_with("msg_"));
        let restored = store.recent(10).await.expect("read transcript");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content, "local only");
    }

    #[tokio::test]
    async fn source_store_identity_is_stable_across_reopen() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("transcript-source.sqlite3");
        let (first, _) = TranscriptStore::open(path.clone(), None)
            .await
            .expect("open transcript");
        let source_store_id = first.source_store_id().to_owned();
        assert!(source_store_id.starts_with("src_"));
        assert_eq!(source_store_id.len(), 36);
        drop(first);

        let (reopened, _) = TranscriptStore::open(path, None)
            .await
            .expect("reopen transcript");
        assert_eq!(reopened.source_store_id(), source_store_id);
    }

    #[tokio::test]
    async fn checkpoints_model_judged_migration_per_pcp_identity() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) =
            TranscriptStore::open(temporary.path().join("transcript-migration.sqlite3"), None)
                .await
                .expect("open transcript");
        for (at, content) in [
            ("2026-08-15T00:00:00Z", "first"),
            ("2026-08-15T00:01:00Z", "second"),
            ("2026-08-15T00:02:00Z", "third"),
        ] {
            store
                .append(
                    entry(MemoryRole::User, at, content),
                    TranscriptMessageLinks::default(),
                )
                .await
                .expect("append migration source");
        }
        let upper = store
            .max_visible_sequence()
            .await
            .expect("migration upper bound");
        assert_eq!(upper, 3);
        let first = store
            .pcp_migration_batch(0, upper, 2)
            .await
            .expect("first migration batch");
        assert_eq!(first.len(), 2);
        assert_eq!(first[1].entry.content, "second");
        store
            .advance_pcp_migration_watermark("idn_new", first[1].sequence)
            .await
            .expect("advance migration");
        assert_eq!(
            store
                .pcp_migration_watermark("idn_new")
                .await
                .expect("read migration watermark"),
            2
        );
        assert_eq!(
            store
                .pcp_migration_watermark("idn_other")
                .await
                .expect("read independent watermark"),
            0
        );
        let remaining = store
            .pcp_migration_batch(2, upper, 2)
            .await
            .expect("remaining migration batch");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].entry.content, "third");
    }

    #[tokio::test]
    async fn imports_an_archived_conversation_once() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archived = temporary.path().join("archived.sqlite3");
        let connection = Connection::open(&archived).expect("create archived snapshot");
        connection
            .execute_batch(
                "
                CREATE TABLE pcp_pages(page_id TEXT PRIMARY KEY, current_revision_id TEXT, kind TEXT NOT NULL);
                CREATE TABLE pcp_revisions(revision_id TEXT PRIMARY KEY, created_at TEXT NOT NULL, observed_at TEXT, payload_content TEXT, facets_json TEXT);
                INSERT INTO pcp_pages(page_id, current_revision_id, kind) VALUES ('pg_1', 'rev_1', 'conversation_event');
                INSERT INTO pcp_revisions(revision_id, created_at, observed_at, payload_content, facets_json)
                  VALUES ('rev_1', '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z', 'hello', '{\"role\":\"user\",\"kind\":\"conversation_event\"}');
                ",
            )
            .expect("seed archived snapshot");
        drop(connection);
        let transcript = temporary.path().join("transcript.sqlite3");
        let (store, report) = TranscriptStore::open(transcript.clone(), Some(archived.clone()))
            .await
            .expect("restore archived transcript");
        assert_eq!(report.imported_messages, 1);
        assert_eq!(
            store.recent(10).await.expect("read restored")[0]
                .revision_id
                .as_deref(),
            Some("rev_1")
        );
        let (_, second) = TranscriptStore::open(transcript, Some(archived))
            .await
            .expect("reopen transcript");
        assert_eq!(second.imported_messages, 0);
    }
}
