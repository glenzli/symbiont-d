use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{Value, json};

use crate::diagnostics::{ExecutionTraceEvent, TraceEventKind, bounded_trace_value};

pub(super) fn observable_item_event(item: &Value) -> Option<(TraceEventKind, &'static str, Value)> {
    match item.get("type").and_then(Value::as_str)? {
        "reasoning" => {
            let summary = item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if summary.is_empty() {
                None
            } else {
                Some((
                    TraceEventKind::ReasoningSummary,
                    "Model reasoning summary",
                    json!({"summary": summary}),
                ))
            }
        }
        "webSearch" => Some((
            TraceEventKind::WebSearch,
            "Live web search",
            json!({
                "query": item.get("query"),
                "action": item.get("action"),
                "results": item.get("results")
            }),
        )),
        "imageGeneration" => Some((
            TraceEventKind::ToolCall,
            "Generated image",
            json!({
                "status": item.get("status"),
                "savedPath": item.get("savedPath"),
                "revisedPrompt": item.get("revisedPrompt")
            }),
        )),
        "contextCompaction" => Some((
            TraceEventKind::ContextCompaction,
            "Codex compacted the native thread context",
            json!({}),
        )),
        _ => None,
    }
}

pub(super) fn observable_history_item(entry: &Value) -> Option<Value> {
    let item = entry.get("item")?;
    let item_type = item.get("type").and_then(Value::as_str)?;
    let mut value = match item_type {
        "userMessage" => json!({
            "type": item_type,
            "content": item.get("content")
        }),
        "agentMessage" => json!({
            "type": item_type,
            "text": item.get("text"),
            "phase": item.get("phase")
        }),
        "reasoning" => json!({
            "type": item_type,
            "summary": item.get("summary")
        }),
        "dynamicToolCall" => json!({
            "type": item_type,
            "namespace": item.get("namespace"),
            "tool": item.get("tool"),
            "status": item.get("status")
        }),
        "webSearch" => json!({
            "type": item_type,
            "query": item.get("query"),
            "action": item.get("action")
        }),
        "contextCompaction" => json!({"type": item_type}),
        _ => return None,
    };
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "turnId".to_owned(),
            entry.get("turnId").cloned().unwrap_or(Value::Null),
        );
    }
    Some(value)
}

pub(super) fn push_trace_event(
    events: &mut Vec<ExecutionTraceEvent>,
    kind: TraceEventKind,
    title: impl Into<String>,
    details: Value,
    occurred_at: String,
) {
    events.push(ExecutionTraceEvent {
        sequence: events.len() as u32,
        kind,
        occurred_at,
        title: title.into(),
        details: bounded_trace_value(details),
    });
}

pub(super) fn timestamp_from_millis(value: Option<i64>) -> String {
    value
        .and_then(|milliseconds| Utc.timestamp_millis_opt(milliseconds).single())
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(now)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
