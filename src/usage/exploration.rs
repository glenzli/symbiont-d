use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::diagnostics::TraceEventKind;

use super::trace::{self, TraceBundle};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationRunSummary {
    pub trace_id: String,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: u64,
    pub status: String,
    pub surfaced: bool,
    pub message: Option<String>,
    pub total_tokens: u64,
    pub model_runs: Vec<ExplorationModelRun>,
    pub reasoning_summaries: Vec<String>,
    pub web_searches: u64,
    pub search_queries: Vec<String>,
    pub pcp_recall_calls: u64,
    pub pcp_write_calls: u64,
    pub details_retained: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationModelRun {
    pub model: String,
    pub display_name: String,
    pub effort: String,
    pub stage: String,
}

pub(super) fn read_recent(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<ExplorationRunSummary>> {
    let mut statement = connection
        .prepare(
            "
            SELECT id
            FROM invocations
            WHERE origin IN ('autonomous_scout', 'autonomous') AND parent_id IS NULL
            ORDER BY started_at DESC
            LIMIT ?1
            ",
        )
        .context("prepare recent exploration query")?;
    let trace_ids = statement
        .query_map(params![limit as i64], |row| row.get::<_, String>(0))
        .context("query recent explorations")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect recent exploration ids")?;

    trace_ids
        .into_iter()
        .filter_map(|trace_id| match trace::read(connection, &trace_id) {
            Ok(Some(bundle)) => Some(Ok(summarize(bundle))),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn summarize(bundle: TraceBundle) -> ExplorationRunSummary {
    let surfaced = bundle.runs.iter().any(|run| run.produced_message);
    let mut message = None;
    let mut agent_message = None;
    let mut reasoning_summaries = Vec::new();
    let mut search_queries = Vec::new();
    let mut web_searches = 0_u64;

    for run in &bundle.runs {
        for step in &run.steps {
            if step.succeeded
                && step.namespace == "symbiont"
                && step.tool == "propose_proactive_message"
            {
                message = step
                    .arguments
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned);
            }
        }
        for event in &run.events {
            match &event.kind {
                TraceEventKind::ReasoningSummary => {
                    if let Some(summaries) = event.details.get("summary").and_then(|v| v.as_array())
                    {
                        reasoning_summaries.extend(
                            summaries
                                .iter()
                                .filter_map(|value| value.as_str())
                                .map(str::to_owned),
                        );
                    }
                }
                TraceEventKind::WebSearch => {
                    web_searches += 1;
                    if let Some(query) = event
                        .details
                        .get("query")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        if !search_queries.iter().any(|existing| existing == query) {
                            search_queries.push(query.to_owned());
                        }
                    }
                }
                TraceEventKind::AgentMessage => {
                    if let Some(text) = event.details.get("text").and_then(|value| value.as_str()) {
                        agent_message = Some(text.to_owned());
                    }
                }
                _ => {}
            }
        }
    }

    let started_at = bundle
        .runs
        .first()
        .map(|run| run.started_at.clone())
        .unwrap_or_default();
    let completed_at = bundle
        .runs
        .last()
        .map(|run| run.completed_at.clone())
        .unwrap_or_default();
    let status = bundle
        .runs
        .iter()
        .find(|run| run.status != "completed")
        .or_else(|| bundle.runs.last())
        .map(|run| run.status.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let duration_ms = bundle.runs.iter().map(|run| run.duration_ms).sum();
    let total_tokens = bundle.runs.iter().map(|run| run.total_tokens).sum();
    let model_runs = bundle
        .runs
        .iter()
        .map(|run| ExplorationModelRun {
            model: run.model.clone(),
            display_name: run.display_name.clone(),
            effort: run.effort.clone(),
            stage: match (run.origin.as_str(), run.parent_id.is_some()) {
                ("autonomous_scout", _) => "scout",
                ("autonomous", true) => "review",
                _ => "explore",
            }
            .to_owned(),
        })
        .collect();

    let message = message.or(agent_message);

    reasoning_summaries.truncate(24);
    search_queries.truncate(12);
    ExplorationRunSummary {
        trace_id: bundle.trace_id,
        started_at,
        completed_at,
        duration_ms,
        status,
        surfaced,
        message: surfaced.then_some(message).flatten(),
        total_tokens,
        model_runs,
        reasoning_summaries,
        web_searches,
        search_queries,
        pcp_recall_calls: bundle.pcp_recall_calls,
        pcp_write_calls: bundle.pcp_write_calls,
        details_retained: bundle.details_retained,
    }
}
