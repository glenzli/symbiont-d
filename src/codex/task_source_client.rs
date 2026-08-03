use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{sleep, timeout},
};
use tracing::{debug, warn};

use super::{
    CodexConfig, CodexTaskDetail, CodexTaskSummary,
    approvals::automatic_server_request_response,
    task_bridge::{parse_task_detail, parse_task_list},
};

/// A deliberately small app-server session used only to inspect existing Codex tasks.
///
/// This does not load models, open a Codex thread, or expose tools. Keeping it apart
/// from `CodexClient` means a user opening the composer picker never waits behind a
/// Symbiont turn or its background maintenance work.
pub(super) struct CodexTaskSourceClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl CodexTaskSourceClient {
    pub async fn start(config: CodexConfig) -> Result<Self> {
        let mut last_error = None;
        for attempt in 1..=2 {
            match timeout(Duration::from_secs(10), Self::start_once(config.clone())).await {
                Ok(Ok(client)) => return Ok(client),
                Ok(Err(error)) => {
                    warn!(attempt, %error, "Codex task source startup attempt failed");
                    last_error = Some(error);
                }
                Err(_) => {
                    warn!(attempt, "Codex task source startup attempt timed out");
                    last_error = Some(anyhow::anyhow!(
                        "Codex task source startup timed out after 10 seconds"
                    ));
                }
            }
            sleep(Duration::from_millis(150)).await;
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Codex task source did not start")))
    }

    async fn start_once(config: CodexConfig) -> Result<Self> {
        let mut child = Command::new(&config.binary)
            .arg("app-server")
            .arg("--stdio")
            .current_dir(&config.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {} app-server for task source", config.binary))?;
        let stdin = child
            .stdin
            .take()
            .context("Codex task source did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex task source did not expose stdout")?;
        let mut client = Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 1,
        };
        client.initialize().await?;
        Ok(client)
    }

    pub async fn list_tasks(&mut self, limit: u32) -> Result<Vec<CodexTaskSummary>> {
        let result = self
            .request(
                "thread/list",
                json!({
                    "limit": limit.clamp(1, 50),
                    "sortKey": "updated_at",
                    "sortDirection": "desc",
                    "sourceKinds": ["cli", "vscode", "appServer"],
                    "archived": false
                }),
            )
            .await
            .context("list interactive Codex tasks")?;
        parse_task_list(&result)
    }

    pub async fn read_task(&mut self, thread_id: &str) -> Result<CodexTaskDetail> {
        if thread_id.trim().is_empty() || thread_id.len() > 128 {
            anyhow::bail!("invalid Codex task id");
        }
        let result = self
            .request(
                "thread/read",
                json!({
                    "threadId": thread_id,
                    "includeTurns": true
                }),
            )
            .await
            .context("read Codex task")?;
        let detail = parse_task_detail(&result)?;
        if detail.task.ephemeral
            || !matches!(detail.task.source.as_str(), "cli" | "vscode" | "appServer")
        {
            anyhow::bail!("only interactive Codex tasks can be read");
        }
        Ok(detail)
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "symbiont_d_task_source",
                    "title": "symbiont-d task source",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true
                }
            }),
        )
        .await
        .context("initialize Codex task source")?;
        self.send_notification("initialized", json!({})).await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let request_id = self.send_request(method, params).await?;
        loop {
            let message = self.read_message().await?;
            if self.handle_server_request(&message).await? {
                tokio::task::yield_now().await;
                continue;
            }
            if message.get("id") != Some(&json!(request_id)) {
                tokio::task::yield_now().await;
                continue;
            }
            if let Some(error) = message.get("error") {
                anyhow::bail!("{method} failed: {error}");
            }
            return message
                .get("result")
                .cloned()
                .with_context(|| format!("{method} response omitted result"));
        }
    }

    async fn handle_server_request(&mut self, message: &Value) -> Result<bool> {
        if let Some(response) = automatic_server_request_response(message) {
            let id = message
                .get("id")
                .cloned()
                .context("server request omitted id")?;
            self.send_json(&json!({
                "id": id,
                "result": response
            }))
            .await?;
            return Ok(true);
        }
        if message.get("method").and_then(Value::as_str) != Some("item/tool/call") {
            return Ok(false);
        }
        let id = message
            .get("id")
            .cloned()
            .context("dynamic tool request omitted id")?;
        self.send_json(&json!({
            "id": id,
            "result": {
                "success": false,
                "contentItems": [{"type": "inputText", "text": "Task sources cannot execute tools."}]
            }
        }))
        .await?;
        Ok(true)
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_json(&json!({
            "method": method,
            "id": id,
            "params": params
        }))
        .await?;
        Ok(id)
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send_json(&json!({
            "method": method,
            "params": params
        }))
        .await
    }

    async fn send_json(&mut self, message: &Value) -> Result<()> {
        let mut encoded = serde_json::to_vec(message).context("encode task source message")?;
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .await
            .context("write to Codex task source")?;
        self.stdin.flush().await.context("flush Codex task source")
    }

    async fn read_message(&mut self) -> Result<Value> {
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .context("read from Codex task source")?
                .context("Codex task source closed its output")?;
            if line.trim().is_empty() {
                continue;
            }
            debug!(message = %line, "received Codex task source message");
            match serde_json::from_str(&line) {
                Ok(message) => return Ok(message),
                Err(error) => warn!(%error, "ignored non-JSON Codex task source output"),
            }
        }
    }
}
