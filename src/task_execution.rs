use std::{io::ErrorKind, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{Mutex, RwLock, mpsc},
};
use tracing::error;

use crate::{
    asset::AssetStore,
    codex::{CodexClient, RuntimeEvent, import_generated_images},
    compute::{ComputeLane, ComputeStore},
    continuity::{ContinuityHost, MessageLinks},
    memory::MemoryRole,
    profile::ProfileStore,
    reflection::ReflectionHandle,
    usage::UsageStore,
};

const MAX_RUNS: usize = 40;
const MAX_INSTRUCTION_CHARS: usize = 12_000;
const MAX_REASON_CHARS: usize = 800;
const MAX_TASK_IMAGES: usize = 4;
const ONE_SHOT_LEASE_MINUTES: i64 = 15;
const TOPIC_LEASE_MINUTES: i64 = 45;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundCodexTask {
    pub thread_id: String,
    pub title: String,
    pub cwd: String,
    pub bound_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLeaseScope {
    OneShot,
    Topic,
    Pinned,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLease {
    pub id: String,
    pub task: BoundCodexTask,
    pub scope: TaskLeaseScope,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub source_revision_id: Option<String>,
}

impl TaskLease {
    pub fn new(task: BoundCodexTask, scope: TaskLeaseScope) -> Self {
        let now = Utc::now();
        let expires_at = match scope {
            TaskLeaseScope::OneShot => Some(now + Duration::minutes(ONE_SHOT_LEASE_MINUTES)),
            TaskLeaseScope::Topic => Some(now + Duration::minutes(TOPIC_LEASE_MINUTES)),
            TaskLeaseScope::Pinned => None,
        };
        Self {
            id: format!("task_lease_{}", now.timestamp_micros()),
            task,
            scope,
            created_at: now.to_rfc3339(),
            expires_at: expires_at.map(|value| value.to_rfc3339()),
            source_revision_id: None,
        }
    }

    pub fn for_turn(mut self, source_revision_id: &str) -> Self {
        self.source_revision_id = Some(source_revision_id.to_owned());
        if self.scope == TaskLeaseScope::Topic {
            self.expires_at =
                Some((Utc::now() + Duration::minutes(TOPIC_LEASE_MINUTES)).to_rfc3339());
        }
        self
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value <= Utc::now())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunPhase {
    Queued,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunSnapshot {
    pub id: String,
    pub task: BoundCodexTask,
    pub instruction: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_revision_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope: Option<TaskLeaseScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision_id: Option<String>,
    pub lane: ComputeLane,
    pub phase: TaskRunPhase,
    pub current_activity: Option<String>,
    pub result_revision_id: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone)]
struct ExecutionGate {
    enabled: bool,
    lease: Option<TaskLease>,
}

impl Default for ExecutionGate {
    fn default() -> Self {
        Self {
            enabled: false,
            lease: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TaskExecutionRequest {
    pub id: String,
    pub task: BoundCodexTask,
    pub instruction: String,
    pub reason: String,
    pub image_revision_ids: Vec<String>,
    pub lane: ComputeLane,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskRunDocument {
    runs: Vec<TaskRunSnapshot>,
}

pub struct TaskExecutionReceiver {
    receiver: mpsc::Receiver<TaskExecutionRequest>,
}

pub struct TaskExecutionQueue {
    path: PathBuf,
    state: RwLock<Vec<TaskRunSnapshot>>,
    gate: RwLock<ExecutionGate>,
    sender: mpsc::Sender<TaskExecutionRequest>,
}

impl TaskExecutionQueue {
    pub async fn open(path: PathBuf) -> Result<(Self, TaskExecutionReceiver)> {
        let mut runs = match fs::read_to_string(&path).await {
            Ok(content) => {
                serde_json::from_str::<TaskRunDocument>(&content)
                    .context("parse Codex task execution journal")?
                    .runs
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read task execution journal {}", path.display()));
            }
        };
        let interrupted_at = Utc::now().to_rfc3339();
        for run in &mut runs {
            if matches!(run.phase, TaskRunPhase::Queued | TaskRunPhase::Running) {
                run.phase = TaskRunPhase::Interrupted;
                run.current_activity = None;
                run.error = Some("symbiont-d restarted before this task completed".to_owned());
                run.completed_at = Some(interrupted_at.clone());
            }
        }
        trim_runs(&mut runs);
        let (sender, receiver) = mpsc::channel(16);
        let queue = Self {
            path,
            state: RwLock::new(runs),
            gate: RwLock::new(ExecutionGate::default()),
            sender,
        };
        queue.persist().await?;
        Ok((queue, TaskExecutionReceiver { receiver }))
    }

    pub async fn configure(&self, enabled: bool, lease: Option<TaskLease>) {
        *self.gate.write().await = ExecutionGate { enabled, lease };
    }

    pub async fn snapshot(&self) -> Vec<TaskRunSnapshot> {
        self.state.read().await.iter().rev().cloned().collect()
    }

    pub async fn enqueue(
        &self,
        instruction: &str,
        reason: &str,
        image_revision_ids: Vec<String>,
        lane: ComputeLane,
    ) -> Result<TaskRunSnapshot> {
        let instruction = instruction.trim();
        let reason = reason.trim();
        validate_request(instruction, reason, &image_revision_ids, lane)?;
        let mut gate = self.gate.write().await;
        if !gate.enabled {
            anyhow::bail!("Codex task execution is not enabled");
        }
        let lease = gate
            .lease
            .take()
            .context("no active Codex task lease is available for this turn")?;
        if lease.is_expired() {
            anyhow::bail!("the selected Codex task lease has expired");
        }
        drop(gate);
        let task = lease.task.clone();
        let now = Utc::now();
        let id = format!("task_run_{}", now.timestamp_micros());
        let run = TaskRunSnapshot {
            id: id.clone(),
            task: task.clone(),
            instruction: instruction.to_owned(),
            reason: reason.to_owned(),
            image_revision_ids: image_revision_ids.clone(),
            target_lease_id: Some(lease.id.clone()),
            target_scope: Some(lease.scope),
            source_revision_id: lease.source_revision_id.clone(),
            lane,
            phase: TaskRunPhase::Queued,
            current_activity: Some("等待 Codex 任务空闲".to_owned()),
            result_revision_id: None,
            error: None,
            created_at: now.to_rfc3339(),
            started_at: None,
            completed_at: None,
        };
        {
            let mut runs = self.state.write().await;
            runs.push(run.clone());
            trim_runs(&mut runs);
        }
        self.persist().await?;
        if self
            .sender
            .send(TaskExecutionRequest {
                id,
                task,
                instruction: instruction.to_owned(),
                reason: reason.to_owned(),
                image_revision_ids,
                lane,
            })
            .await
            .is_err()
        {
            self.fail(&run.id, "task execution worker is unavailable")
                .await?;
            anyhow::bail!("task execution worker is unavailable");
        }
        Ok(run)
    }

    async fn start_run(&self, id: &str) -> Result<()> {
        self.update(id, |run| {
            run.phase = TaskRunPhase::Running;
            run.current_activity = Some("正在接入 Codex 任务".to_owned());
            run.started_at = Some(Utc::now().to_rfc3339());
            run.error = None;
        })
        .await
    }

    async fn set_activity(&self, id: &str, activity: String) -> Result<()> {
        self.update(id, |run| {
            if run.phase == TaskRunPhase::Running {
                run.current_activity = Some(activity);
            }
        })
        .await
    }

    async fn complete(&self, id: &str, revision_id: String) -> Result<()> {
        self.update(id, |run| {
            run.phase = TaskRunPhase::Completed;
            run.current_activity = None;
            run.result_revision_id = Some(revision_id);
            run.error = None;
            run.completed_at = Some(Utc::now().to_rfc3339());
        })
        .await
    }

    async fn fail(&self, id: &str, message: &str) -> Result<()> {
        self.update(id, |run| {
            run.phase = TaskRunPhase::Failed;
            run.current_activity = None;
            run.error = Some(message.to_owned());
            run.completed_at = Some(Utc::now().to_rfc3339());
        })
        .await
    }

    async fn update(&self, id: &str, update: impl FnOnce(&mut TaskRunSnapshot)) -> Result<()> {
        {
            let mut runs = self.state.write().await;
            let run = runs
                .iter_mut()
                .find(|run| run.id == id)
                .with_context(|| format!("unknown task execution run {id}"))?;
            update(run);
        }
        self.persist().await
    }

    async fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create task execution directory {}", parent.display()))?;
        }
        let content = serde_json::to_vec_pretty(&TaskRunDocument {
            runs: self.state.read().await.clone(),
        })
        .context("encode task execution journal")?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write task execution journal {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace task execution journal {}", self.path.display()))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_worker(
    mut receiver: TaskExecutionReceiver,
    queue: Arc<TaskExecutionQueue>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    assets: Arc<AssetStore>,
    reflection: ReflectionHandle,
    usage: Arc<UsageStore>,
) {
    tokio::spawn(async move {
        while let Some(request) = receiver.receiver.recv().await {
            if let Err(worker_error) = run_one(
                Arc::clone(&queue),
                Arc::clone(&codex),
                Arc::clone(&compute),
                Arc::clone(&profile),
                Arc::clone(&continuity),
                Arc::clone(&assets),
                reflection.clone(),
                Arc::clone(&usage),
                request.clone(),
            )
            .await
            {
                error!(run_id = %request.id, error = %worker_error, "bound Codex task failed");
                let _ = queue.fail(&request.id, &worker_error.to_string()).await;
                let _ = publish_failure(&continuity, &reflection, &request, &worker_error).await;
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_one(
    queue: Arc<TaskExecutionQueue>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    assets: Arc<AssetStore>,
    reflection: ReflectionHandle,
    usage: Arc<UsageStore>,
    request: TaskExecutionRequest,
) -> Result<()> {
    queue.start_run(&request.id).await?;
    let (runtime_tx, mut runtime_rx) = mpsc::channel(32);
    let activity_queue = Arc::clone(&queue);
    let activity_run_id = request.id.clone();
    let activity_task = tokio::spawn(async move {
        while let Some(event) = runtime_rx.recv().await {
            let activity = match event {
                RuntimeEvent::Activity { label, .. } => Some(label),
                RuntimeEvent::Reset => Some("正在切换到更深的处理".to_owned()),
                RuntimeEvent::Delta { .. } => None,
            };
            if let Some(activity) = activity {
                let _ = activity_queue
                    .set_activity(&activity_run_id, activity)
                    .await;
            }
        }
    });
    let compute = compute.snapshot().await;
    let profile = profile.snapshot().await;
    let selected_images = continuity
        .read_image_assets(&request.image_revision_ids)
        .await?;
    let mut local_images = Vec::with_capacity(selected_images.len());
    for image in selected_images {
        local_images.push(assets.local_path(&image.attachment.asset_id).await?);
    }
    let outcome = codex
        .lock()
        .await
        .execute_bound_task(&request, &local_images, &compute, &profile, runtime_tx)
        .await;
    activity_task
        .await
        .context("join selected task activity relay")?;
    let mut outcome = outcome?;
    for invocation in &mut outcome.invocations {
        invocation.produced_message = true;
    }
    usage.record_all(&outcome.invocations).await?;
    let generated_images = import_generated_images(&assets, &outcome.generated_images).await?;
    let content = if outcome.text.trim().is_empty() {
        format!(
            "我已经在 Codex 任务「{}」里处理了这一步。",
            request.task.title
        )
    } else {
        format!(
            "我已经在 Codex 任务「{}」里处理了这一步。\n\n{}",
            request.task.title,
            outcome.text.trim()
        )
    };
    let stored = continuity
        .ingest_message(
            MemoryRole::Assistant,
            &content,
            generated_images,
            Some(outcome.metadata),
            MessageLinks {
                responds_to: None,
                continues_from: None,
                input_revision_ids: request.image_revision_ids.clone(),
                surfaced_hunch_revision_ids: Vec::new(),
                quotes: Vec::new(),
                topic: None,
            },
        )
        .await?;
    reflection.record_message(&stored.entry, None, &[]).await?;
    queue
        .complete(&request.id, stored.page.revision_id.clone())
        .await
}

async fn publish_failure(
    continuity: &ContinuityHost,
    reflection: &ReflectionHandle,
    request: &TaskExecutionRequest,
    worker_error: &anyhow::Error,
) -> Result<()> {
    let content = format!(
        "Codex 任务「{}」没有完成这次操作：{}",
        request.task.title, worker_error
    );
    let stored = continuity
        .ingest_message(
            MemoryRole::Assistant,
            &content,
            Vec::new(),
            None,
            MessageLinks {
                responds_to: None,
                continues_from: None,
                input_revision_ids: Vec::new(),
                surfaced_hunch_revision_ids: Vec::new(),
                quotes: Vec::new(),
                topic: None,
            },
        )
        .await?;
    reflection.record_message(&stored.entry, None, &[]).await
}

fn validate_request(
    instruction: &str,
    reason: &str,
    image_revision_ids: &[String],
    lane: ComputeLane,
) -> Result<()> {
    if instruction.is_empty() || instruction.chars().count() > MAX_INSTRUCTION_CHARS {
        anyhow::bail!("task instruction must contain 1-{MAX_INSTRUCTION_CHARS} characters");
    }
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        anyhow::bail!("task reason must contain 1-{MAX_REASON_CHARS} characters");
    }
    if image_revision_ids.len() > MAX_TASK_IMAGES {
        anyhow::bail!("a selected task can receive at most {MAX_TASK_IMAGES} images");
    }
    let mut unique_images = std::collections::HashSet::new();
    if image_revision_ids.iter().any(|revision_id| {
        revision_id.trim().is_empty()
            || revision_id.len() > 128
            || !unique_images.insert(revision_id.as_str())
    }) {
        anyhow::bail!("selected task image Revision IDs must be non-empty and unique");
    }
    if !matches!(lane, ComputeLane::Investigate | ComputeLane::Critical) {
        anyhow::bail!("selected task execution requires investigate or critical compute");
    }
    Ok(())
}

fn trim_runs(runs: &mut Vec<TaskRunSnapshot>) {
    if runs.len() > MAX_RUNS {
        runs.drain(..runs.len() - MAX_RUNS);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("symbiont-task-runs-{name}-{nonce}.json"))
    }

    fn task() -> BoundCodexTask {
        BoundCodexTask {
            thread_id: "thread-1".to_owned(),
            title: "symbiont-d".to_owned(),
            cwd: "/tmp/symbiont-d".to_owned(),
            bound_at: Utc::now().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn execution_requires_an_enabled_task_lease() {
        let (queue, _receiver) = TaskExecutionQueue::open(test_path("gate")).await.unwrap();
        assert!(
            queue
                .enqueue(
                    "Implement it",
                    "The user requested it",
                    Vec::new(),
                    ComputeLane::Investigate
                )
                .await
                .is_err()
        );
        queue
            .configure(
                true,
                Some(TaskLease::new(task(), TaskLeaseScope::OneShot).for_turn("user-revision-1")),
            )
            .await;
        let run = queue
            .enqueue(
                "Implement it",
                "The user requested it",
                vec!["rev_image".to_owned()],
                ComputeLane::Investigate,
            )
            .await
            .unwrap();
        assert_eq!(run.phase, TaskRunPhase::Queued);
        assert_eq!(run.task.thread_id, "thread-1");
        assert_eq!(run.image_revision_ids, vec!["rev_image"]);
        assert_eq!(run.target_scope, Some(TaskLeaseScope::OneShot));
        assert_eq!(run.source_revision_id.as_deref(), Some("user-revision-1"));
        assert!(
            queue
                .enqueue(
                    "Run a second operation",
                    "The first lease was already spent",
                    Vec::new(),
                    ComputeLane::Investigate
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn unfinished_runs_become_interrupted_after_restart() {
        let path = test_path("restart");
        let (queue, _receiver) = TaskExecutionQueue::open(path.clone()).await.unwrap();
        queue
            .configure(
                true,
                Some(TaskLease::new(task(), TaskLeaseScope::Topic).for_turn("user-revision-2")),
            )
            .await;
        queue
            .enqueue(
                "Implement it",
                "The user requested it",
                Vec::new(),
                ComputeLane::Critical,
            )
            .await
            .unwrap();
        drop(queue);

        let (reopened, _receiver) = TaskExecutionQueue::open(path).await.unwrap();
        assert_eq!(
            reopened.snapshot().await[0].phase,
            TaskRunPhase::Interrupted
        );
    }
}
