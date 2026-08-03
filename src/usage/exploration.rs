use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::diagnostics::TraceEventKind;
use crate::outreach::{OutreachKind, PROPOSE_OUTREACH_TOOL};

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
    pub outreach_kind: Option<OutreachKind>,
    pub message: Option<String>,
    pub focus: Option<ExplorationFocusSummary>,
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
pub struct ExplorationFocusSummary {
    pub title: String,
    pub detail: Option<String>,
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
    let mut outreach_kind = None;
    let mut agent_message = None;
    let mut reasoning_summaries = Vec::new();
    let mut search_queries = Vec::new();
    let mut web_searches = 0_u64;
    let mut finding_focus = None;

    for run in &bundle.runs {
        for step in &run.steps {
            if step.succeeded && step.namespace == "symbiont" && step.tool == PROPOSE_OUTREACH_TOOL
            {
                outreach_kind = Some(OutreachKind::from_arguments(&step.arguments));
                message = step
                    .arguments
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned);
            }
            if step.succeeded
                && step.namespace == "symbiont"
                && step.tool == "submit_exploration_finding"
            {
                let title = compact_argument(&step.arguments, "topic", 180);
                let detail = compact_argument(&step.arguments, "claim", 360);
                if let Some(title) = title {
                    finding_focus = Some(ExplorationFocusSummary {
                        detail: detail.filter(|detail| detail != &title),
                        title,
                    });
                }
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
    let focus = finding_focus.or_else(|| fallback_focus(&search_queries, &reasoning_summaries));
    ExplorationRunSummary {
        trace_id: bundle.trace_id,
        started_at,
        completed_at,
        duration_ms,
        status,
        surfaced,
        outreach_kind: surfaced.then_some(outreach_kind).flatten(),
        message: surfaced.then_some(message).flatten(),
        focus,
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

fn fallback_focus(
    search_queries: &[String],
    reasoning_summaries: &[String],
) -> Option<ExplorationFocusSummary> {
    if let Some(title) = search_queries.first() {
        let detail = search_queries
            .iter()
            .skip(1)
            .take(2)
            .map(|query| compact_text(query, 180))
            .collect::<Vec<_>>()
            .join("；");
        return Some(ExplorationFocusSummary {
            title: compact_text(title, 220),
            detail: (!detail.is_empty()).then_some(detail),
        });
    }
    reasoning_summaries
        .iter()
        .map(|summary| compact_text(summary, 360))
        .find(|summary| !summary.is_empty())
        .map(|title| ExplorationFocusSummary {
            title,
            detail: None,
        })
}

fn compact_argument(
    arguments: &serde_json::Value,
    field: &str,
    max_chars: usize,
) -> Option<String> {
    arguments
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(|value| compact_text(value, max_chars))
        .filter(|value| !value.is_empty())
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut compact = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    compact.push('…');
    compact
}
