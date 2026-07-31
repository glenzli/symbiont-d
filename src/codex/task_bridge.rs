use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

const MAX_TASK_MESSAGES: usize = 16;
const MAX_TASK_CONTENT_CHARS: usize = 9_000;
const MAX_TASK_MESSAGE_CHARS: usize = 3_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTaskSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub cwd: String,
    pub source: String,
    pub ephemeral: bool,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTaskMessage {
    pub role: String,
    pub text: String,
    pub at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTaskDetail {
    pub task: CodexTaskSummary,
    pub messages: Vec<CodexTaskMessage>,
    pub truncated: bool,
}

pub(crate) fn parse_task_list(result: &Value) -> Result<Vec<CodexTaskSummary>> {
    result
        .get("data")
        .and_then(Value::as_array)
        .context("thread/list response omitted data")?
        .iter()
        .filter(|thread| {
            !thread
                .get("ephemeral")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(parse_task_summary)
        .collect()
}

pub(crate) fn parse_task_detail(result: &Value) -> Result<CodexTaskDetail> {
    let thread = result
        .get("thread")
        .context("thread/read response omitted thread")?;
    let task = parse_task_summary(thread)?;
    let turns = thread
        .get("turns")
        .and_then(Value::as_array)
        .context("thread/read response omitted turns")?;
    let mut messages = Vec::new();
    let mut content_truncated = false;
    for turn in turns {
        let user_at = turn.get("startedAt").and_then(Value::as_i64);
        let assistant_at = turn.get("completedAt").and_then(Value::as_i64).or(user_at);
        for item in turn
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("userMessage") => {
                    let text = item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|content| {
                            content.get("type").and_then(Value::as_str) == Some("text")
                        })
                        .filter_map(|content| content.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    content_truncated |= push_message(&mut messages, "user", &text, user_at);
                }
                Some("agentMessage") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        content_truncated |=
                            push_message(&mut messages, "assistant", text, assistant_at);
                    }
                }
                _ => {}
            }
        }
    }
    let original_len = messages.len();
    let original_chars: usize = messages
        .iter()
        .map(|message| message.text.chars().count())
        .sum();
    let messages = bounded_tail(messages);
    Ok(CodexTaskDetail {
        task,
        truncated: content_truncated
            || original_len > messages.len()
            || original_chars > MAX_TASK_CONTENT_CHARS,
        messages,
    })
}

fn parse_task_summary(value: &Value) -> Result<CodexTaskSummary> {
    let id = required_text(value, "id")?;
    let preview = value
        .get("preview")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let title = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&preview)
        .trim()
        .to_owned();
    Ok(CodexTaskSummary {
        id,
        title: if title.is_empty() {
            "Untitled Codex task".to_owned()
        } else {
            title
        },
        preview,
        cwd: value
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        source: source_label(value.get("source")),
        ephemeral: value
            .get("ephemeral")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status: value
            .pointer("/status/type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
    })
}

fn required_text(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("Codex task omitted {key}"))
}

fn source_label(source: Option<&Value>) -> String {
    match source {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Object(value)) if value.contains_key("custom") => value
            .get("custom")
            .and_then(Value::as_str)
            .unwrap_or("custom")
            .to_owned(),
        Some(Value::Object(value)) if value.contains_key("subAgent") => "subAgent".to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn push_message(
    messages: &mut Vec<CodexTaskMessage>,
    role: &str,
    text: &str,
    at: Option<i64>,
) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let truncated = text.chars().count() > MAX_TASK_MESSAGE_CHARS;
    messages.push(CodexTaskMessage {
        role: role.to_owned(),
        text: truncate(text, MAX_TASK_MESSAGE_CHARS),
        at,
    });
    truncated
}

fn bounded_tail(messages: Vec<CodexTaskMessage>) -> Vec<CodexTaskMessage> {
    let mut kept = Vec::new();
    let mut chars = 0;
    for message in messages.into_iter().rev() {
        let message_chars = message.text.chars().count();
        if kept.len() >= MAX_TASK_MESSAGES
            || (!kept.is_empty() && chars + message_chars > MAX_TASK_CONTENT_CHARS)
        {
            break;
        }
        chars += message_chars;
        kept.push(message);
    }
    kept.reverse();
    kept
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MAX_TASK_MESSAGE_CHARS, parse_task_detail, parse_task_list};

    #[test]
    fn parses_task_list_without_loading_turns() {
        let tasks = parse_task_list(&json!({
            "data": [
                {
                    "id": "thread-1",
                    "name": "Bridge design",
                    "preview": "Discuss a bridge",
                    "cwd": "/tmp/project",
                    "source": "vscode",
                    "ephemeral": false,
                    "status": {"type": "idle"},
                    "createdAt": 10,
                    "updatedAt": 20
                },
                {
                    "id": "thread-background",
                    "name": "Internal background work",
                    "preview": "",
                    "cwd": "/tmp/project",
                    "source": "appServer",
                    "ephemeral": true,
                    "status": {"type": "idle"},
                    "createdAt": 10,
                    "updatedAt": 30
                }
            ]
        }))
        .expect("parse task list");

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Bridge design");
        assert_eq!(tasks[0].source, "vscode");
        assert!(!tasks[0].ephemeral);
    }

    #[test]
    fn keeps_only_user_and_agent_messages_in_task_detail() {
        let detail = parse_task_detail(&json!({
            "thread": {
                "id": "thread-1",
                "preview": "Discuss a bridge",
                "cwd": "/tmp/project",
                "source": "cli",
                "status": {"type": "notLoaded"},
                "createdAt": 10,
                "updatedAt": 20,
                "turns": [{
                    "startedAt": 11,
                    "completedAt": 12,
                    "items": [
                        {"type": "userMessage", "content": [
                            {"type": "text", "text": "hello"},
                            {"type": "localImage", "path": "/tmp/image.png"}
                        ]},
                        {"type": "reasoning", "summary": ["private"]},
                        {"type": "agentMessage", "text": "world"}
                    ]
                }]
            }
        }))
        .expect("parse task detail");

        assert_eq!(detail.messages.len(), 2);
        assert_eq!(detail.messages[0].role, "user");
        assert_eq!(detail.messages[0].text, "hello");
        assert_eq!(detail.messages[1].role, "assistant");
        assert_eq!(detail.messages[1].text, "world");
    }

    #[test]
    fn truncates_oversized_task_messages() {
        let detail = parse_task_detail(&json!({
            "thread": {
                "id": "thread-1",
                "preview": "",
                "cwd": "",
                "source": "cli",
                "status": {"type": "notLoaded"},
                "createdAt": 10,
                "updatedAt": 20,
                "turns": [{
                    "items": [{
                        "type": "agentMessage",
                        "text": "x".repeat(MAX_TASK_MESSAGE_CHARS + 50)
                    }]
                }]
            }
        }))
        .expect("parse task detail");

        assert!(detail.truncated);
        assert_eq!(
            detail.messages[0].text.chars().count(),
            MAX_TASK_MESSAGE_CHARS
        );
    }
}
