use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::working_context::WorkingContext;

pub const TRACE_RETENTION_DAYS: i64 = 7;
pub const TRACE_RETENTION_INVOCATIONS: usize = 128;
const TRACE_VALUE_MAX_CHARS: usize = 96_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub input: Vec<Value>,
    pub fragments: Vec<ContextFragment>,
    pub working_context: Option<WorkingContext>,
    pub developer_instructions: String,
    pub native_thread: NativeThreadSnapshot,
    #[serde(default)]
    pub selection: Vec<crate::context_assembly::ContextSelection>,
    /// Exact client-side request values, not a reconstructed provider prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted: Option<SubmittedContext>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmittedContext {
    pub thread_start: Value,
    pub turn_start: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFragment {
    pub source: String,
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeThreadSnapshot {
    pub thread_id: String,
    pub cursor_before: Option<String>,
    pub prior_turns: u64,
    pub compactions_before: u64,
    pub model_context_window: Option<u64>,
    pub exact_prompt_available: bool,
    pub observable_history_tail: Vec<Value>,
    pub history_tail_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTraceEvent {
    pub sequence: u32,
    pub kind: TraceEventKind,
    pub occurred_at: String,
    pub title: String,
    pub details: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TraceEventKind {
    ReasoningSummary,
    ToolCall,
    WebSearch,
    ContextCompaction,
    ThreadRollover,
    ModelReroute,
    PermissionRequest,
    PermissionResolution,
    TurnInterrupted,
    TurnSettled,
    AgentMessage,
}

pub fn bounded_trace_value(value: Value) -> Value {
    let Ok(encoded) = serde_json::to_string(&value) else {
        return json!({"truncated": true, "reason": "could not encode trace value"});
    };
    if encoded.chars().count() <= TRACE_VALUE_MAX_CHARS {
        return value;
    }
    let preview = encoded
        .chars()
        .take(TRACE_VALUE_MAX_CHARS)
        .collect::<String>();
    json!({
        "truncated": true,
        "originalChars": encoded.chars().count(),
        "jsonPreview": preview
    })
}
