use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task;

use crate::diagnostics::{ContextSnapshot, ExecutionTraceEvent, bounded_trace_value};

#[path = "usage/exploration.rs"]
mod exploration;
#[path = "usage/trace.rs"]
mod trace;

pub use exploration::ExplorationRunSummary;
pub use trace::TraceBundle;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTraceStep {
    pub sequence: u32,
    pub namespace: String,
    pub tool: String,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: u64,
    pub succeeded: bool,
    pub arguments: Value,
    pub result: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvocationRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub thread_id: String,
    pub turn_id: String,
    pub origin: String,
    pub lane: String,
    pub requested_model: String,
    pub effective_model: String,
    pub model_display_name: String,
    pub effort: String,
    pub service_tier: Option<String>,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: u64,
    pub status: String,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    pub tool_calls: Vec<String>,
    pub produced_message: bool,
    #[serde(skip)]
    pub trace_steps: Vec<ToolTraceStep>,
    #[serde(skip)]
    pub context_snapshot: Option<ContextSnapshot>,
    #[serde(skip)]
    pub trace_events: Vec<ExecutionTraceEvent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub totals: UsageTotals,
    pub by_model: Vec<ModelUsage>,
    pub recent: Vec<InvocationRecord>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub invocations: u64,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub display_name: String,
    pub invocations: u64,
    pub total_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageHeadline {
    pub total_tokens: u64,
    pub autonomous_tokens_today: u64,
    pub autonomous_messages_today: u64,
    pub autonomous_interventions_today: u64,
    pub autonomous_notes_today: u64,
    pub reflection_tokens_today: u64,
}

pub struct UsageStore {
    path: PathBuf,
}

impl UsageStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let path_for_open = path.clone();
        task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create usage directory {}", parent.display()))?;
            }
            let connection = Connection::open(&path_for_open)
                .with_context(|| format!("open usage database {}", path_for_open.display()))?;
            connection
                .execute_batch(
                    "
                PRAGMA journal_mode = WAL;
                CREATE TABLE IF NOT EXISTS invocations (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    thread_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL UNIQUE,
                    origin TEXT NOT NULL,
                    lane TEXT NOT NULL,
                    requested_model TEXT NOT NULL,
                    effective_model TEXT NOT NULL,
                    model_display_name TEXT NOT NULL,
                    effort TEXT NOT NULL,
                    service_tier TEXT,
                    started_at TEXT NOT NULL,
                    completed_at TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL,
                    status TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL,
                    cached_input_tokens INTEGER NOT NULL,
                    output_tokens INTEGER NOT NULL,
                    reasoning_output_tokens INTEGER NOT NULL,
                    total_tokens INTEGER NOT NULL,
                    tool_calls_json TEXT NOT NULL,
                    produced_message INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS invocations_completed_at
                    ON invocations(completed_at DESC);
                CREATE INDEX IF NOT EXISTS invocations_model
                    ON invocations(effective_model);
                CREATE TABLE IF NOT EXISTS invocation_tool_trace (
                    invocation_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    namespace TEXT NOT NULL,
                    tool TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    completed_at TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL,
                    succeeded INTEGER NOT NULL,
                    arguments_json TEXT NOT NULL,
                    result_json TEXT NOT NULL,
                    PRIMARY KEY (invocation_id, sequence)
                );
                CREATE INDEX IF NOT EXISTS invocation_tool_trace_invocation
                    ON invocation_tool_trace(invocation_id, sequence);
                CREATE TABLE IF NOT EXISTS invocation_context (
                    invocation_id TEXT PRIMARY KEY,
                    snapshot_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS invocation_trace_event (
                    invocation_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    event_json TEXT NOT NULL,
                    PRIMARY KEY (invocation_id, sequence)
                );
                CREATE INDEX IF NOT EXISTS invocation_trace_event_invocation
                    ON invocation_trace_event(invocation_id, sequence);
                ",
                )
                .context("initialize usage database")?;
            Ok(())
        })
        .await
        .context("join usage database initialization")??;
        Ok(Self { path })
    }

    pub async fn record_all(&self, records: &[InvocationRecord]) -> Result<()> {
        let path = self.path.clone();
        let records = records.to_vec();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = Connection::open(&path)
                .with_context(|| format!("open usage database {}", path.display()))?;
            let transaction = connection
                .transaction()
                .context("start usage transaction")?;
            for record in records {
                transaction
                    .execute(
                        "
                        INSERT OR REPLACE INTO invocations (
                            id, parent_id, thread_id, turn_id, origin, lane,
                            requested_model, effective_model, model_display_name,
                            effort, service_tier, started_at, completed_at,
                            duration_ms, status, input_tokens, cached_input_tokens,
                            output_tokens, reasoning_output_tokens, total_tokens,
                            tool_calls_json, produced_message
                        ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                            ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
                        )
                        ",
                        params![
                            &record.id,
                            &record.parent_id,
                            &record.thread_id,
                            &record.turn_id,
                            &record.origin,
                            &record.lane,
                            &record.requested_model,
                            &record.effective_model,
                            &record.model_display_name,
                            &record.effort,
                            &record.service_tier,
                            &record.started_at,
                            &record.completed_at,
                            record.duration_ms as i64,
                            &record.status,
                            record.input_tokens as i64,
                            record.cached_input_tokens as i64,
                            record.output_tokens as i64,
                            record.reasoning_output_tokens as i64,
                            record.total_tokens as i64,
                            serde_json::to_string(&record.tool_calls)
                                .context("encode tool call names")?,
                            if record.produced_message {
                                1_i64
                            } else {
                                0_i64
                            },
                        ],
                    )
                    .context("record invocation")?;
                transaction
                    .execute(
                        "DELETE FROM invocation_tool_trace WHERE invocation_id = ?1",
                        params![&record.id],
                    )
                    .context("replace invocation tool trace")?;
                for step in &record.trace_steps {
                    transaction
                        .execute(
                            "
                            INSERT INTO invocation_tool_trace (
                                invocation_id, sequence, namespace, tool,
                                started_at, completed_at, duration_ms, succeeded,
                                arguments_json, result_json
                            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                            ",
                            params![
                                &record.id,
                                step.sequence as i64,
                                &step.namespace,
                                &step.tool,
                                &step.started_at,
                                &step.completed_at,
                                step.duration_ms as i64,
                                if step.succeeded { 1_i64 } else { 0_i64 },
                                serde_json::to_string(&bounded_trace_value(step.arguments.clone()))
                                    .context("encode tool trace arguments")?,
                                serde_json::to_string(&bounded_trace_value(step.result.clone()))
                                    .context("encode tool trace result")?,
                            ],
                        )
                        .context("record invocation tool trace")?;
                }
                transaction
                    .execute(
                        "DELETE FROM invocation_context WHERE invocation_id = ?1",
                        params![&record.id],
                    )
                    .context("replace invocation context")?;
                if let Some(snapshot) = &record.context_snapshot {
                    transaction
                        .execute(
                            "
                            INSERT INTO invocation_context (invocation_id, snapshot_json)
                            VALUES (?1, ?2)
                            ",
                            params![
                                &record.id,
                                serde_json::to_string(snapshot)
                                    .context("encode invocation context")?
                            ],
                        )
                        .context("record invocation context")?;
                }
                transaction
                    .execute(
                        "DELETE FROM invocation_trace_event WHERE invocation_id = ?1",
                        params![&record.id],
                    )
                    .context("replace invocation trace events")?;
                for event in &record.trace_events {
                    transaction
                        .execute(
                            "
                            INSERT INTO invocation_trace_event (
                                invocation_id, sequence, event_json
                            ) VALUES (?1, ?2, ?3)
                            ",
                            params![
                                &record.id,
                                event.sequence as i64,
                                serde_json::to_string(event)
                                    .context("encode invocation trace event")?
                            ],
                        )
                        .context("record invocation trace event")?;
                }
            }
            trace::prune(&transaction)?;
            transaction.commit().context("commit usage transaction")
        })
        .await
        .context("join usage record write")?
    }

    pub async fn summary(&self) -> Result<UsageSummary> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<UsageSummary> {
            let connection = Connection::open(&path)
                .with_context(|| format!("open usage database {}", path.display()))?;
            let totals = connection
                .query_row(
                    "
                    SELECT COUNT(*), COALESCE(SUM(total_tokens), 0),
                           COALESCE(SUM(input_tokens), 0),
                           COALESCE(SUM(cached_input_tokens), 0),
                           COALESCE(SUM(output_tokens), 0),
                           COALESCE(SUM(reasoning_output_tokens), 0),
                           COALESCE(SUM(duration_ms), 0)
                    FROM invocations
                    ",
                    [],
                    |row| {
                        Ok(UsageTotals {
                            invocations: row.get::<_, i64>(0)? as u64,
                            total_tokens: row.get::<_, i64>(1)? as u64,
                            input_tokens: row.get::<_, i64>(2)? as u64,
                            cached_input_tokens: row.get::<_, i64>(3)? as u64,
                            output_tokens: row.get::<_, i64>(4)? as u64,
                            reasoning_output_tokens: row.get::<_, i64>(5)? as u64,
                            duration_ms: row.get::<_, i64>(6)? as u64,
                        })
                    },
                )
                .context("read usage totals")?;

            let mut by_model_statement = connection
                .prepare(
                    "
                    SELECT effective_model, model_display_name, COUNT(*),
                           COALESCE(SUM(total_tokens), 0),
                           COALESCE(SUM(reasoning_output_tokens), 0),
                           COALESCE(SUM(duration_ms), 0)
                    FROM invocations
                    GROUP BY effective_model, model_display_name
                    ORDER BY SUM(total_tokens) DESC
                    ",
                )
                .context("prepare model usage query")?;
            let by_model = by_model_statement
                .query_map([], |row| {
                    Ok(ModelUsage {
                        model: row.get(0)?,
                        display_name: row.get(1)?,
                        invocations: row.get::<_, i64>(2)? as u64,
                        total_tokens: row.get::<_, i64>(3)? as u64,
                        reasoning_output_tokens: row.get::<_, i64>(4)? as u64,
                        duration_ms: row.get::<_, i64>(5)? as u64,
                    })
                })
                .context("query model usage")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect model usage")?;

            let mut recent_statement = connection
                .prepare(
                    "
                    SELECT id, parent_id, thread_id, turn_id, origin, lane,
                           requested_model, effective_model, model_display_name,
                           effort, service_tier, started_at, completed_at,
                           duration_ms, status, input_tokens, cached_input_tokens,
                           output_tokens, reasoning_output_tokens, total_tokens,
                           tool_calls_json, produced_message
                    FROM invocations
                    ORDER BY completed_at DESC
                    LIMIT 50
                    ",
                )
                .context("prepare recent invocation query")?;
            let recent = recent_statement
                .query_map([], invocation_from_row)
                .context("query recent invocations")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect recent invocations")?;

            Ok(UsageSummary {
                totals,
                by_model,
                recent,
            })
        })
        .await
        .context("join usage summary read")?
    }

    pub async fn headline(&self, today_started_at: &str) -> Result<UsageHeadline> {
        let path = self.path.clone();
        let today_started_at = today_started_at.to_owned();
        task::spawn_blocking(move || -> Result<UsageHeadline> {
            let connection = Connection::open(&path)
                .with_context(|| format!("open usage database {}", path.display()))?;
            connection
                .query_row(
                    "
                    SELECT
                        COALESCE(SUM(total_tokens), 0),
                        COALESCE(SUM(
                            CASE
                                WHEN origin IN (
                                    'ambient_sense', 'autonomous', 'autonomous_scout', 'maintenance', 'pcp_maintenance',
                                    'reconciliation_preview', 'reconciliation_apply'
                                )
                                     AND completed_at >= ?1
                                THEN total_tokens ELSE 0
                            END
                        ), 0),
                        COALESCE(SUM(
                            CASE
                                WHEN origin IN (
                                    'ambient_sense', 'autonomous', 'autonomous_scout', 'maintenance', 'pcp_maintenance', 'reflection',
                                    'reconciliation_preview', 'reconciliation_apply'
                                )
                                     AND completed_at >= ?1
                                     AND produced_message = 1
                                THEN 1 ELSE 0
                            END
                        ), 0),
                        COALESCE(SUM(
                            CASE
                                WHEN origin IN (
                                    'ambient_sense', 'autonomous', 'autonomous_scout', 'maintenance', 'pcp_maintenance', 'reflection',
                                    'reconciliation_preview', 'reconciliation_apply'
                                )
                                     AND completed_at >= ?1
                                     AND produced_message = 1
                                     AND EXISTS (
                                        SELECT 1
                                        FROM invocation_tool_trace AS outreach
                                        WHERE outreach.invocation_id = invocations.id
                                          AND outreach.namespace = 'symbiont'
                                          AND outreach.tool = 'propose_proactive_message'
                                          AND json_extract(outreach.arguments_json, '$.kind') = 'note'
                                     )
                                THEN 1 ELSE 0
                            END
                        ), 0),
                        COALESCE(SUM(
                            CASE
                                WHEN origin IN (
                                    'reflection', 'pcp_maintenance',
                                    'reconciliation_preview', 'reconciliation_apply'
                                )
                                     AND completed_at >= ?1
                                THEN total_tokens ELSE 0
                            END
                        ), 0)
                    FROM invocations
                    ",
                    params![today_started_at],
                    |row| {
                        Ok(UsageHeadline {
                            total_tokens: row.get::<_, i64>(0)? as u64,
                            autonomous_tokens_today: row.get::<_, i64>(1)? as u64,
                            autonomous_messages_today: row.get::<_, i64>(2)? as u64,
                            autonomous_notes_today: row.get::<_, i64>(3)? as u64,
                            autonomous_interventions_today: row.get::<_, i64>(2)? as u64
                                - row.get::<_, i64>(3)? as u64,
                            reflection_tokens_today: row.get::<_, i64>(4)? as u64,
                        })
                    },
                )
                .context("read usage headline")
        })
        .await
        .context("join usage headline read")?
    }

    pub async fn latest_exploration_completed_at(&self) -> Result<Option<String>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Option<String>> {
            let connection = Connection::open(&path)
                .with_context(|| format!("open usage database {}", path.display()))?;
            connection
                .query_row(
                    "
                    SELECT MAX(completed_at)
                    FROM invocations
                    WHERE origin IN ('ambient_sense', 'autonomous_scout', 'autonomous')
                      AND parent_id IS NULL
                    ",
                    [],
                    |row| row.get(0),
                )
                .context("read latest autonomous exploration completion")
        })
        .await
        .context("join latest autonomous exploration read")?
    }

    pub async fn trace(&self, trace_id: &str) -> Result<Option<TraceBundle>> {
        let path = self.path.clone();
        let trace_id = trace_id.to_owned();
        task::spawn_blocking(move || -> Result<Option<TraceBundle>> {
            let connection = Connection::open(&path)
                .with_context(|| format!("open usage database {}", path.display()))?;
            trace::read(&connection, &trace_id)
        })
        .await
        .context("join invocation trace read")?
    }

    pub async fn recent_explorations(&self, limit: usize) -> Result<Vec<ExplorationRunSummary>> {
        let path = self.path.clone();
        task::spawn_blocking(move || -> Result<Vec<ExplorationRunSummary>> {
            let connection = Connection::open(&path)
                .with_context(|| format!("open usage database {}", path.display()))?;
            exploration::read_recent(&connection, limit.clamp(1, 20))
        })
        .await
        .context("join recent exploration read")?
    }
}

fn invocation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvocationRecord> {
    let tool_calls_json: String = row.get(20)?;
    Ok(InvocationRecord {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        thread_id: row.get(2)?,
        turn_id: row.get(3)?,
        origin: row.get(4)?,
        lane: row.get(5)?,
        requested_model: row.get(6)?,
        effective_model: row.get(7)?,
        model_display_name: row.get(8)?,
        effort: row.get(9)?,
        service_tier: row.get(10)?,
        started_at: row.get(11)?,
        completed_at: row.get(12)?,
        duration_ms: row.get::<_, i64>(13)? as u64,
        status: row.get(14)?,
        input_tokens: row.get::<_, i64>(15)? as u64,
        cached_input_tokens: row.get::<_, i64>(16)? as u64,
        output_tokens: row.get::<_, i64>(17)? as u64,
        reasoning_output_tokens: row.get::<_, i64>(18)? as u64,
        total_tokens: row.get::<_, i64>(19)? as u64,
        tool_calls: serde_json::from_str(&tool_calls_json).unwrap_or_default(),
        produced_message: row.get::<_, i64>(21)? != 0,
        trace_steps: Vec::new(),
        context_snapshot: None,
        trace_events: Vec::new(),
    })
}

#[cfg(test)]
#[path = "usage/tests.rs"]
mod tests;
