use anyhow::{Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::diagnostics::{
    ContextSnapshot, ExecutionTraceEvent, TRACE_RETENTION_DAYS, TRACE_RETENTION_INVOCATIONS,
};

use super::{InvocationRecord, ToolTraceStep, invocation_from_row};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceBundle {
    pub trace_id: String,
    pub runs: Vec<TraceRun>,
    pub pcp_tool_calls: u64,
    pub pcp_recall_calls: u64,
    pub pcp_write_calls: u64,
    pub event_count: u64,
    pub details_retained: bool,
    pub retention_days: i64,
    pub retention_invocations: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRun {
    pub invocation_id: String,
    pub parent_id: Option<String>,
    pub origin: String,
    pub lane: String,
    pub model: String,
    pub display_name: String,
    pub effort: String,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: u64,
    pub status: String,
    pub produced_message: bool,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub context: Option<ContextSnapshot>,
    pub events: Vec<ExecutionTraceEvent>,
    pub steps: Vec<ToolTraceStep>,
}

pub(super) fn read(connection: &Connection, trace_id: &str) -> Result<Option<TraceBundle>> {
    let Some(root_trace_id) = connection
        .query_row(
            "
            SELECT COALESCE(parent_id, id)
            FROM invocations
            WHERE id = ?1
            ",
            params![trace_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("resolve root invocation trace")?
    else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "
            SELECT id, parent_id, thread_id, turn_id, origin, lane,
                   requested_model, effective_model, model_display_name,
                   effort, service_tier, started_at, completed_at,
                   duration_ms, status, input_tokens, cached_input_tokens,
                   output_tokens, reasoning_output_tokens, total_tokens,
                   tool_calls_json, produced_message
            FROM invocations
            WHERE id = ?1 OR parent_id = ?1
            ORDER BY started_at ASC
            ",
        )
        .context("prepare invocation trace query")?;
    let invocations = statement
        .query_map(params![&root_trace_id], invocation_from_row)
        .context("query invocation trace")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect invocation trace")?;
    if invocations.is_empty() {
        return Ok(None);
    }

    let mut runs = Vec::with_capacity(invocations.len());
    let mut pcp_tool_calls = 0_u64;
    let mut pcp_recall_calls = 0_u64;
    let mut pcp_write_calls = 0_u64;
    let mut event_count = 0_u64;
    let mut details_retained = false;
    for invocation in invocations {
        let steps = read_steps(connection, &invocation)?;
        let context = connection
            .query_row(
                "
                SELECT snapshot_json
                FROM invocation_context
                WHERE invocation_id = ?1
                ",
                params![&invocation.id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("query invocation context")?
            .and_then(|value| serde_json::from_str(&value).ok());
        let events = read_events(connection, &invocation)?;
        pcp_tool_calls += steps.iter().filter(|step| step.namespace == "pcp").count() as u64;
        pcp_recall_calls += steps.iter().filter(|step| is_pcp_recall(step)).count() as u64;
        pcp_write_calls += steps.iter().filter(|step| is_pcp_write(step)).count() as u64;
        event_count += events.len() as u64;
        details_retained |= context.is_some() || !events.is_empty() || !steps.is_empty();
        runs.push(TraceRun {
            invocation_id: invocation.id,
            parent_id: invocation.parent_id,
            origin: invocation.origin,
            lane: invocation.lane,
            model: invocation.effective_model,
            display_name: invocation.model_display_name,
            effort: invocation.effort,
            started_at: invocation.started_at,
            completed_at: invocation.completed_at,
            duration_ms: invocation.duration_ms,
            status: invocation.status,
            produced_message: invocation.produced_message,
            total_tokens: invocation.total_tokens,
            input_tokens: invocation.input_tokens,
            cached_input_tokens: invocation.cached_input_tokens,
            output_tokens: invocation.output_tokens,
            reasoning_output_tokens: invocation.reasoning_output_tokens,
            context,
            events,
            steps,
        });
    }
    Ok(Some(TraceBundle {
        trace_id: root_trace_id,
        runs,
        pcp_tool_calls,
        pcp_recall_calls,
        pcp_write_calls,
        event_count,
        details_retained,
        retention_days: TRACE_RETENTION_DAYS,
        retention_invocations: TRACE_RETENTION_INVOCATIONS,
    }))
}

fn is_pcp_recall(step: &ToolTraceStep) -> bool {
    step.namespace == "pcp" && matches!(step.tool.as_str(), "search_pages" | "read_pages")
}

fn is_pcp_write(step: &ToolTraceStep) -> bool {
    step.namespace == "pcp"
        && matches!(
            step.tool.as_str(),
            "write_summary" | "write_page" | "revise_page" | "link_pages"
        )
}

pub(super) fn prune(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    let cutoff = (Utc::now() - Duration::days(TRACE_RETENTION_DAYS))
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let keep = TRACE_RETENTION_INVOCATIONS as i64;
    for statement in [
        "
        DELETE FROM invocation_tool_trace
        WHERE invocation_id NOT IN (
            SELECT id FROM invocations
            WHERE completed_at >= ?1
            ORDER BY completed_at DESC
            LIMIT ?2
        )
        ",
        "
        DELETE FROM invocation_context
        WHERE invocation_id NOT IN (
            SELECT id FROM invocations
            WHERE completed_at >= ?1
            ORDER BY completed_at DESC
            LIMIT ?2
        )
        ",
        "
        DELETE FROM invocation_trace_event
        WHERE invocation_id NOT IN (
            SELECT id FROM invocations
            WHERE completed_at >= ?1
            ORDER BY completed_at DESC
            LIMIT ?2
        )
        ",
    ] {
        transaction
            .execute(statement, params![&cutoff, keep])
            .context("prune expired invocation trace details")?;
    }
    Ok(())
}

fn read_steps(
    connection: &Connection,
    invocation: &InvocationRecord,
) -> Result<Vec<ToolTraceStep>> {
    let mut statement = connection
        .prepare(
            "
            SELECT sequence, namespace, tool, started_at, completed_at,
                   duration_ms, succeeded, arguments_json, result_json
            FROM invocation_tool_trace
            WHERE invocation_id = ?1
            ORDER BY sequence ASC
            ",
        )
        .context("prepare tool trace query")?;
    statement
        .query_map(params![&invocation.id], tool_trace_from_row)
        .context("query tool trace")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect tool trace")
}

fn read_events(
    connection: &Connection,
    invocation: &InvocationRecord,
) -> Result<Vec<ExecutionTraceEvent>> {
    let mut statement = connection
        .prepare(
            "
            SELECT event_json
            FROM invocation_trace_event
            WHERE invocation_id = ?1
            ORDER BY sequence ASC
            ",
        )
        .context("prepare invocation event query")?;
    Ok(statement
        .query_map(params![&invocation.id], |row| row.get::<_, String>(0))
        .context("query invocation events")?
        .filter_map(|value| {
            value
                .ok()
                .and_then(|json| serde_json::from_str::<ExecutionTraceEvent>(&json).ok())
        })
        .collect())
}

fn tool_trace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolTraceStep> {
    let arguments_json: String = row.get(7)?;
    let result_json: String = row.get(8)?;
    Ok(ToolTraceStep {
        sequence: row.get::<_, i64>(0)? as u32,
        namespace: row.get(1)?,
        tool: row.get(2)?,
        started_at: row.get(3)?,
        completed_at: row.get(4)?,
        duration_ms: row.get::<_, i64>(5)? as u64,
        succeeded: row.get::<_, i64>(6)? != 0,
        arguments: serde_json::from_str(&arguments_json).unwrap_or(Value::Null),
        result: serde_json::from_str(&result_json).unwrap_or(Value::Null),
    })
}
