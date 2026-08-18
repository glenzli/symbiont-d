//! Local, user-owned state for Symbiont itself.
//!
//! Chat transcripts have their own store.  This database holds the small
//! revisable working model that Symbiont maintains for itself, together with a
//! lossless local archive of the non-transcript records recovered from the
//! pre-v0.8 PCP snapshot.  PCP is deliberately not a dependency of either
//! read or write operation after import.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use getrandom::fill;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use tokio::{sync::Mutex, task};

const SCHEMA_VERSION: &str = "1";

#[derive(Clone, Debug, Default)]
pub struct StateRestoreReport {
    pub imported_records: usize,
    pub imported_relations: usize,
    pub imported_provenance: usize,
    pub source_snapshot: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct LocalContextDocument {
    pub kind: String,
    pub content: String,
    pub document_id: String,
    pub revision_id: String,
    pub updated_at: String,
    pub source_revision_ids: Vec<String>,
    pub facets: Option<Value>,
}

pub struct SymbiontStateStore {
    path: PathBuf,
    mutation: Arc<Mutex<()>>,
}

impl SymbiontStateStore {
    pub async fn open(
        path: PathBuf,
        legacy_snapshot: Option<PathBuf>,
    ) -> Result<(Self, StateRestoreReport)> {
        let store = Self {
            path,
            mutation: Arc::new(Mutex::new(())),
        };
        store.initialize().await?;
        let report = store.restore_legacy_archive(legacy_snapshot).await?;
        Ok((store, report))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read_context(&self, stable_key: &str) -> Result<Option<LocalContextDocument>> {
        let path = self.path.clone();
        let stable_key = stable_key.to_owned();
        task::spawn_blocking(move || read_context_blocking(&path, &stable_key))
            .await
            .context("join local context read")?
    }

    pub async fn upsert_context(
        &self,
        stable_key: &str,
        kind: &str,
        content: &str,
        source_revision_ids: Vec<String>,
        facets: Option<Value>,
    ) -> Result<(LocalContextDocument, bool, bool)> {
        let _guard = self.mutation.lock().await;
        let path = self.path.clone();
        let stable_key = stable_key.to_owned();
        let kind = kind.to_owned();
        let content = content.to_owned();
        task::spawn_blocking(move || {
            upsert_context_blocking(
                &path,
                &stable_key,
                &kind,
                &content,
                source_revision_ids,
                facets,
            )
        })
        .await
        .context("join local context write")?
    }

    async fn initialize(&self) -> Result<()> {
        let path = self.path.clone();
        task::spawn_blocking(move || initialize_database(&path))
            .await
            .context("join local state initialization")?
    }

    async fn restore_legacy_archive(
        &self,
        legacy_snapshot: Option<PathBuf>,
    ) -> Result<StateRestoreReport> {
        let Some(snapshot) = legacy_snapshot.filter(|path| path.is_file()) else {
            return Ok(StateRestoreReport::default());
        };
        let _guard = self.mutation.lock().await;
        let destination = self.path.clone();
        let source = snapshot.clone();
        task::spawn_blocking(move || restore_legacy_archive_blocking(&destination, &source))
            .await
            .context("join legacy local-state import")?
    }

    #[cfg(test)]
    pub fn for_test(path: PathBuf) -> Self {
        initialize_database(&path).expect("initialize test state store");
        Self {
            path,
            mutation: Arc::new(Mutex::new(())),
        }
    }
}

fn initialize_database(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create local state directory {}", parent.display()))?;
    }
    let connection = open_connection(path)?;
    connection.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS state_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS state_context_documents (
            stable_key TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            content TEXT NOT NULL,
            document_id TEXT NOT NULL UNIQUE,
            revision_id TEXT NOT NULL UNIQUE,
            updated_at TEXT NOT NULL,
            source_revision_ids_json TEXT NOT NULL,
            facets_json TEXT
        );
        CREATE TABLE IF NOT EXISTS state_legacy_records (
            source_snapshot TEXT NOT NULL,
            source_revision_id TEXT NOT NULL,
            source_page_id TEXT NOT NULL,
            namespace TEXT NOT NULL,
            kind TEXT NOT NULL,
            mutability TEXT NOT NULL,
            lifecycle_status TEXT NOT NULL,
            is_current INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            observed_at TEXT,
            content TEXT,
            source_refs_json TEXT NOT NULL,
            facets_json TEXT,
            provenance_json TEXT NOT NULL,
            previous_revision_id TEXT,
            imported_at TEXT NOT NULL,
            PRIMARY KEY(source_snapshot, source_revision_id)
        );
        CREATE INDEX IF NOT EXISTS state_legacy_records_kind
            ON state_legacy_records(namespace, kind, created_at);
        CREATE TABLE IF NOT EXISTS state_legacy_relations (
            source_snapshot TEXT NOT NULL,
            relation_id TEXT NOT NULL,
            from_revision_id TEXT NOT NULL,
            relation_type TEXT NOT NULL,
            to_revision_id TEXT NOT NULL,
            actor_type TEXT NOT NULL,
            actor_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            from_page_id TEXT NOT NULL,
            to_page_id TEXT NOT NULL,
            basis_revision_ids_json TEXT NOT NULL,
            imported_at TEXT NOT NULL,
            PRIMARY KEY(source_snapshot, relation_id)
        );
        CREATE TABLE IF NOT EXISTS state_legacy_provenance_inputs (
            source_snapshot TEXT NOT NULL,
            derived_revision_id TEXT NOT NULL,
            input_revision_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            imported_at TEXT NOT NULL,
            PRIMARY KEY(source_snapshot, derived_revision_id, input_revision_id)
        );
        ",
    )?;
    connection.execute(
        "INSERT OR REPLACE INTO state_meta(key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}

fn read_context_blocking(path: &Path, stable_key: &str) -> Result<Option<LocalContextDocument>> {
    let connection = open_connection(path)?;
    connection
        .query_row(
            "SELECT kind, content, document_id, revision_id, updated_at,
                    source_revision_ids_json, facets_json
             FROM state_context_documents WHERE stable_key = ?1",
            [stable_key],
            |row| {
                let sources: String = row.get(5)?;
                let facets: Option<String> = row.get(6)?;
                Ok(LocalContextDocument {
                    kind: row.get(0)?,
                    content: row.get(1)?,
                    document_id: row.get(2)?,
                    revision_id: row.get(3)?,
                    updated_at: row.get(4)?,
                    source_revision_ids: serde_json::from_str(&sources).map_err(to_sql_error)?,
                    facets: facets
                        .map(|value| serde_json::from_str(&value).map_err(to_sql_error))
                        .transpose()?,
                })
            },
        )
        .optional()
        .context("read local context document")
}

fn upsert_context_blocking(
    path: &Path,
    stable_key: &str,
    kind: &str,
    content: &str,
    mut source_revision_ids: Vec<String>,
    facets: Option<Value>,
) -> Result<(LocalContextDocument, bool, bool)> {
    source_revision_ids.retain(|revision| !revision.trim().is_empty());
    source_revision_ids.sort();
    source_revision_ids.dedup();
    let sources = serde_json::to_string(&source_revision_ids)?;
    let facets_json = facets.as_ref().map(serde_json::to_string).transpose()?;
    let connection = open_connection(path)?;
    let existing = read_context_blocking(path, stable_key)?;
    if let Some(existing) = existing {
        if existing.content.trim() == content.trim()
            && existing.kind == kind
            && existing.source_revision_ids == source_revision_ids
            && existing.facets == facets
        {
            return Ok((existing, false, false));
        }
        let updated = LocalContextDocument {
            kind: kind.to_owned(),
            content: content.to_owned(),
            document_id: existing.document_id,
            revision_id: new_id("ctxrev"),
            updated_at: now(),
            source_revision_ids,
            facets,
        };
        connection.execute(
            "UPDATE state_context_documents
             SET kind=?2, content=?3, revision_id=?4, updated_at=?5,
                 source_revision_ids_json=?6, facets_json=?7
             WHERE stable_key=?1",
            params![
                stable_key,
                updated.kind,
                updated.content,
                updated.revision_id,
                updated.updated_at,
                sources,
                facets_json,
            ],
        )?;
        return Ok((updated, false, true));
    }
    let inserted = LocalContextDocument {
        kind: kind.to_owned(),
        content: content.to_owned(),
        document_id: new_id("ctx"),
        revision_id: new_id("ctxrev"),
        updated_at: now(),
        source_revision_ids,
        facets,
    };
    connection.execute(
        "INSERT INTO state_context_documents(
            stable_key, kind, content, document_id, revision_id, updated_at,
            source_revision_ids_json, facets_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            stable_key,
            inserted.kind,
            inserted.content,
            inserted.document_id,
            inserted.revision_id,
            inserted.updated_at,
            sources,
            facets_json,
        ],
    )?;
    Ok((inserted, true, true))
}

fn restore_legacy_archive_blocking(
    destination: &Path,
    source: &Path,
) -> Result<StateRestoreReport> {
    let source_connection = Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open archived PCP snapshot {}", source.display()))?;
    let mut destination_connection = open_connection(destination)?;
    let transaction = destination_connection.transaction()?;
    let source_name = source.display().to_string();
    let imported_at = now();
    let mut report = StateRestoreReport {
        source_snapshot: Some(source.to_path_buf()),
        ..StateRestoreReport::default()
    };

    let mut records = source_connection.prepare(
        "SELECT r.revision_id, r.page_id, r.namespace, p.kind, p.mutability,
                r.lifecycle_status, CASE WHEN p.current_revision_id=r.revision_id THEN 1 ELSE 0 END,
                r.created_at, r.observed_at, r.payload_content, r.source_refs_json,
                r.facets_json, r.provenance_json, r.previous_revision_id
         FROM pcp_revisions r JOIN pcp_pages p ON p.page_id=r.page_id
         WHERE p.kind <> 'conversation_event'
         ORDER BY r.created_at, r.revision_id",
    )?;
    let mut rows = records.query([])?;
    while let Some(row) = rows.next()? {
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO state_legacy_records(
                source_snapshot, source_revision_id, source_page_id, namespace, kind,
                mutability, lifecycle_status, is_current, created_at, observed_at, content,
                source_refs_json, facets_json, provenance_json, previous_revision_id, imported_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                source_name,
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, Option<String>>(13)?,
                imported_at,
            ],
        )?;
        report.imported_records += changed;
    }

    let mut relations = source_connection.prepare(
        "SELECT relation_id, from_revision_id, relation_type, to_revision_id,
                actor_type, actor_id, created_at, from_page_id, to_page_id, basis_revision_ids_json
         FROM pcp_relations ORDER BY created_at, relation_id",
    )?;
    let mut rows = relations.query([])?;
    while let Some(row) = rows.next()? {
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO state_legacy_relations(
                source_snapshot, relation_id, from_revision_id, relation_type, to_revision_id,
                actor_type, actor_id, created_at, from_page_id, to_page_id,
                basis_revision_ids_json, imported_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                source_name,
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                imported_at,
            ],
        )?;
        report.imported_relations += changed;
    }

    let mut provenance = source_connection.prepare(
        "SELECT derived_revision_id, input_revision_id, created_at
         FROM pcp_provenance_inputs ORDER BY created_at, derived_revision_id, input_revision_id",
    )?;
    let mut rows = provenance.query([])?;
    while let Some(row) = rows.next()? {
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO state_legacy_provenance_inputs(
                source_snapshot, derived_revision_id, input_revision_id, created_at, imported_at
             ) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                source_name,
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                imported_at,
            ],
        )?;
        report.imported_provenance += changed;
    }
    transaction.commit()?;
    Ok(report)
}

fn open_connection(path: &Path) -> Result<Connection> {
    Connection::open(path).with_context(|| format!("open local state database {}", path.display()))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn new_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes).expect("secure local state identifier");
    let encoded = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}_{encoded}")
}

fn to_sql_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::SymbiontStateStore;

    #[tokio::test]
    async fn imports_every_non_transcript_record_once_and_preserves_links() {
        let root = tempfile::tempdir().expect("temporary directory");
        let archive = root.path().join("legacy.sqlite3");
        let source = Connection::open(&archive).expect("open archive");
        source.execute_batch(
            "
            CREATE TABLE pcp_pages(page_id TEXT PRIMARY KEY, current_revision_id TEXT, kind TEXT, mutability TEXT);
            CREATE TABLE pcp_revisions(revision_id TEXT PRIMARY KEY, page_id TEXT, namespace TEXT, lifecycle_status TEXT, created_at TEXT, observed_at TEXT, payload_content TEXT, source_refs_json TEXT, facets_json TEXT, provenance_json TEXT, previous_revision_id TEXT);
            CREATE TABLE pcp_relations(relation_id TEXT PRIMARY KEY, from_revision_id TEXT, relation_type TEXT, to_revision_id TEXT, actor_type TEXT, actor_id TEXT, created_at TEXT, from_page_id TEXT, to_page_id TEXT, basis_revision_ids_json TEXT);
            CREATE TABLE pcp_provenance_inputs(derived_revision_id TEXT, input_revision_id TEXT, created_at TEXT);
            INSERT INTO pcp_pages VALUES ('page_keep', 'rev_keep', 'research-map', 'revisioned');
            INSERT INTO pcp_pages VALUES ('page_chat', 'rev_chat', 'conversation_event', 'immutable');
            INSERT INTO pcp_revisions VALUES ('rev_keep', 'page_keep', 'project:symbiont-d', 'active', '2026-08-15T00:00:00Z', NULL, '# Research', '[]', '{\"kind\":\"research-map\"}', '[]', NULL);
            INSERT INTO pcp_revisions VALUES ('rev_chat', 'page_chat', 'conversation:symbiont-d-main', 'active', '2026-08-15T00:01:00Z', NULL, 'chat', '[]', NULL, '[]', NULL);
            INSERT INTO pcp_relations VALUES ('rel_keep', 'rev_keep', 'derived_from', 'rev_chat', 'tool', 'symbiont-d', '2026-08-15T00:02:00Z', 'page_keep', 'page_chat', '[]');
            INSERT INTO pcp_provenance_inputs VALUES ('rev_keep', 'rev_chat', '2026-08-15T00:02:00Z');
            ",
        ).expect("seed archive");
        drop(source);

        let state_path = root.path().join("state.sqlite3");
        let (_state, report) = SymbiontStateStore::open(state_path.clone(), Some(archive))
            .await
            .expect("import state");
        assert_eq!(report.imported_records, 1);
        assert_eq!(report.imported_relations, 1);
        assert_eq!(report.imported_provenance, 1);

        let check = Connection::open(state_path).expect("open state");
        assert_eq!(
            check
                .query_row("SELECT COUNT(*) FROM state_legacy_records", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .expect("count records"),
            1
        );
        assert_eq!(
            check
                .query_row("SELECT COUNT(*) FROM state_legacy_relations", [], |row| row
                    .get::<_, i64>(0))
                .expect("count relations"),
            1
        );
    }

    #[tokio::test]
    async fn revises_local_context_without_pcp() {
        let root = tempfile::tempdir().expect("temporary directory");
        let (state, _) = SymbiontStateStore::open(root.path().join("state.sqlite3"), None)
            .await
            .expect("open state");
        let (first, created, changed) = state
            .upsert_context(
                "symbiont.current_map",
                "symbiont_current_map",
                "# Current\n\nFirst state.",
                vec!["local_message_1".to_owned()],
                None,
            )
            .await
            .expect("write context");
        assert!(created);
        assert!(changed);
        let (second, created, changed) = state
            .upsert_context(
                "symbiont.current_map",
                "symbiont_current_map",
                "# Current\n\nRevised state.",
                vec!["local_message_2".to_owned()],
                None,
            )
            .await
            .expect("revise context");
        assert!(changed);
        assert!(!created);
        assert_eq!(first.document_id, second.document_id);
        assert_ne!(first.revision_id, second.revision_id);
        assert_eq!(
            state
                .read_context("symbiont.current_map")
                .await
                .expect("read context")
                .expect("context")
                .content,
            "# Current\n\nRevised state."
        );
    }
}
