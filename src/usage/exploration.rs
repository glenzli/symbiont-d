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
    pub scope: String,
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
    pub sensing_candidate_count: usize,
    pub sensing_reviewed: bool,
    pub sensing_broadcast_count: usize,
    pub sensing_investigate_count: usize,
    pub sensing_hold_count: usize,
    pub sensing_discard_count: usize,
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
            SELECT root.id
            FROM invocations root
            WHERE root.activity IN ('sensing', 'exploration')
              AND root.parent_id IS NULL
              AND (
                    root.activity != 'sensing'
                    OR NOT EXISTS (
                        SELECT 1
                        FROM invocations legacy_scout
                        WHERE legacy_scout.activity = 'exploration'
                          AND legacy_scout.stage = 'scout'
                          AND legacy_scout.parent_id IS NULL
                          AND legacy_scout.started_at >= root.completed_at
                          AND julianday(legacy_scout.started_at)
                                <= julianday(root.completed_at) + (120.0 / 86400.0)
                    )
              )
            ORDER BY root.started_at DESC
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
    let mut sensing_focus = None;
    let mut fetch_focus = None;
    let mut sensing_candidate_count = 0_usize;
    let mut sensing_reviewed = false;
    let mut sensing_broadcast_count = 0_usize;
    let mut sensing_investigate_count = 0_usize;
    let mut sensing_hold_count = 0_usize;
    let mut sensing_discard_count = 0_usize;

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
            if step.succeeded
                && step.namespace == "symbiont"
                && step.tool == "submit_sensing_candidates"
                && let Some(candidates) = step
                    .arguments
                    .get("candidates")
                    .and_then(serde_json::Value::as_array)
            {
                sensing_candidate_count += candidates.len();
                if sensing_focus.is_none()
                    && let Some(candidate) = candidates.first()
                    && let Some(title) = compact_argument(candidate, "title", 180)
                {
                    sensing_focus = Some(ExplorationFocusSummary {
                        detail: compact_argument(candidate, "summary", 360)
                            .filter(|detail| detail != &title),
                        title,
                    });
                }
            }
            if step.succeeded
                && step.namespace == "symbiont"
                && step.tool == "review_sensing_candidates"
            {
                sensing_reviewed = true;
                if let Some(decisions) = step
                    .arguments
                    .get("decisions")
                    .and_then(serde_json::Value::as_array)
                {
                    for decision in decisions {
                        match decision
                            .get("disposition")
                            .and_then(serde_json::Value::as_str)
                        {
                            Some("broadcast") => sensing_broadcast_count += 1,
                            Some("investigate") => sensing_investigate_count += 1,
                            Some("hold") => sensing_hold_count += 1,
                            Some("discard") => sensing_discard_count += 1,
                            _ => {}
                        }
                    }
                }
            }
            if step.namespace == "symbiont" && step.tool == "fetch_url" {
                web_searches += 1;
                if let Some(purpose) = compact_argument(&step.arguments, "purpose", 220) {
                    if !search_queries.iter().any(|existing| existing == &purpose) {
                        search_queries.push(purpose.clone());
                    }
                    if fetch_focus.is_none() {
                        fetch_focus = Some(ExplorationFocusSummary {
                            title: purpose,
                            detail: compact_argument(&step.arguments, "url", 280),
                        });
                    }
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
            stage: run.stage.clone(),
        })
        .collect();

    let message = message.or(agent_message);

    reasoning_summaries.truncate(24);
    search_queries.truncate(12);
    let scope = if bundle.runs.iter().any(|run| run.activity == "exploration") {
        "exploration"
    } else {
        "sensing"
    };
    let focus = finding_focus
        .or(sensing_focus)
        .or(fetch_focus)
        .or_else(|| fallback_focus(&search_queries, &reasoning_summaries));
    ExplorationRunSummary {
        trace_id: bundle.trace_id,
        scope: scope.to_owned(),
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
        sensing_candidate_count,
        sensing_reviewed,
        sensing_broadcast_count,
        sensing_investigate_count,
        sensing_hold_count,
        sensing_discard_count,
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
