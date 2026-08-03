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
    codex::{CodexClient, RuntimeEvent},
    compute::{ComputeLane, ComputeStore},
    continuity::ContinuityHost,
    profile::ProfileStore,
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
pub struct BoundProject {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub selected_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLeaseScope {
    OneShot,
    Topic,
    Pinned,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLease {
    pub id: String,
    pub project: BoundProject,
    pub scope: ProjectLeaseScope,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub source_revision_id: Option<String>,
}

impl ProjectLease {
    pub fn new(project: BoundProject, scope: ProjectLeaseScope) -> Self {
        let now = Utc::now();
        let expires_at = match scope {
            ProjectLeaseScope::OneShot => Some(now + Duration::minutes(ONE_SHOT_LEASE_MINUTES)),
            ProjectLeaseScope::Topic => Some(now + Duration::minutes(TOPIC_LEASE_MINUTES)),
            ProjectLeaseScope::Pinned => None,
        };
        Self {
            id: format!("project_lease_{}", now.timestamp_micros()),
            project,
            scope,
            created_at: now.to_rfc3339(),
            expires_at: expires_at.map(|value| value.to_rfc3339()),
            source_revision_id: None,
        }
    }

    pub fn for_turn(mut self, source_revision_id: &str) -> Self {
        self.source_revision_id = Some(source_revision_id.to_owned());
        if self.scope == ProjectLeaseScope::Topic {
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
pub enum ProjectHandoffPhase {
    Queued,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectHandoffSnapshot {
    pub id: String,
    pub project: BoundProject,
    pub instruction: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_revision_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<ProjectLeaseScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision_id: Option<String>,
    pub lane: ComputeLane,
    pub phase: ProjectHandoffPhase,
    pub current_activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_task_title: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone)]
struct ProjectHandoffGate {
    enabled: bool,
    lease: Option<ProjectLease>,
}

impl Default for ProjectHandoffGate {
    fn default() -> Self {
        Self {
            enabled: false,
            lease: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProjectHandoffRequest {
    pub id: String,
    pub project: BoundProject,
    pub instruction: String,
    pub reason: String,
    pub image_revision_ids: Vec<String>,
    pub lane: ComputeLane,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectHandoffDocument {
    runs: Vec<ProjectHandoffSnapshot>,
}

pub struct ProjectHandoffReceiver {
    receiver: mpsc::Receiver<ProjectHandoffRequest>,
}

pub struct ProjectHandoffQueue {
    path: PathBuf,
    state: RwLock<Vec<ProjectHandoffSnapshot>>,
    gate: RwLock<ProjectHandoffGate>,
    sender: mpsc::Sender<ProjectHandoffRequest>,
}

impl ProjectHandoffQueue {
    pub async fn open(path: PathBuf) -> Result<(Self, ProjectHandoffReceiver)> {
        let mut runs = match fs::read_to_string(&path).await {
            Ok(content) => {
                serde_json::from_str::<ProjectHandoffDocument>(&content)
                    .context("parse Codex project handoff journal")?
                    .runs
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read Codex project handoff journal {}", path.display())
                });
            }
        };
        let interrupted_at = Utc::now().to_rfc3339();
        for run in &mut runs {
            if matches!(
                run.phase,
                ProjectHandoffPhase::Queued | ProjectHandoffPhase::Running
            ) {
                run.phase = ProjectHandoffPhase::Interrupted;
                run.current_activity = None;
                run.error =
                    Some("symbiont-d restarted before this Codex handoff completed".to_owned());
                run.completed_at = Some(interrupted_at.clone());
            }
        }
        trim_runs(&mut runs);
        let (sender, receiver) = mpsc::channel(16);
        let queue = Self {
            path,
            state: RwLock::new(runs),
            gate: RwLock::new(ProjectHandoffGate::default()),
            sender,
        };
        queue.persist().await?;
        Ok((queue, ProjectHandoffReceiver { receiver }))
    }

    pub async fn configure(&self, enabled: bool, lease: Option<ProjectLease>) {
        *self.gate.write().await = ProjectHandoffGate { enabled, lease };
    }

    pub async fn snapshot(&self) -> Vec<ProjectHandoffSnapshot> {
        self.state.read().await.iter().rev().cloned().collect()
    }

    pub async fn enqueue(
        &self,
        instruction: &str,
        reason: &str,
        image_revision_ids: Vec<String>,
        lane: ComputeLane,
    ) -> Result<ProjectHandoffSnapshot> {
        let instruction = instruction.trim();
        let reason = reason.trim();
        validate_request(instruction, reason, &image_revision_ids, lane)?;
        let mut gate = self.gate.write().await;
        if !gate.enabled {
            anyhow::bail!("Codex project handoffs are not enabled");
        }
        let lease = gate
            .lease
            .take()
            .context("no active project lease is available for this turn")?;
        if lease.is_expired() {
            anyhow::bail!("the selected project lease has expired");
        }
        drop(gate);
        let project = lease.project.clone();
        let now = Utc::now();
        let id = format!("handoff_{}", now.timestamp_micros());
        let run = ProjectHandoffSnapshot {
            id: id.clone(),
            project: project.clone(),
            instruction: instruction.to_owned(),
            reason: reason.to_owned(),
            image_revision_ids: image_revision_ids.clone(),
            project_lease_id: Some(lease.id.clone()),
            project_scope: Some(lease.scope),
            source_revision_id: lease.source_revision_id.clone(),
            lane,
            phase: ProjectHandoffPhase::Queued,
            current_activity: Some("等待创建 Codex 任务".to_owned()),
            codex_task_id: None,
            codex_task_title: None,
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
            .send(ProjectHandoffRequest {
                id,
                project,
                instruction: instruction.to_owned(),
                reason: reason.to_owned(),
                image_revision_ids,
                lane,
            })
            .await
            .is_err()
        {
            self.fail(&run.id, "project handoff worker is unavailable")
                .await?;
            anyhow::bail!("project handoff worker is unavailable");
        }
        Ok(run)
    }

    async fn start_run(&self, id: &str) -> Result<()> {
        self.update(id, |run| {
            run.phase = ProjectHandoffPhase::Running;
            run.current_activity = Some("正在创建 Codex 任务".to_owned());
            run.started_at = Some(Utc::now().to_rfc3339());
            run.error = None;
        })
        .await
    }

    async fn set_activity(&self, id: &str, activity: String) -> Result<()> {
        self.update(id, |run| {
            if run.phase == ProjectHandoffPhase::Running {
                run.current_activity = Some(activity);
            }
        })
        .await
    }

    async fn complete(&self, id: &str, task_id: String, task_title: String) -> Result<()> {
        self.update(id, |run| {
            run.phase = ProjectHandoffPhase::Completed;
            run.current_activity = None;
            run.codex_task_id = Some(task_id);
            run.codex_task_title = Some(task_title);
            run.error = None;
            run.completed_at = Some(Utc::now().to_rfc3339());
        })
        .await
    }

    async fn fail(&self, id: &str, message: &str) -> Result<()> {
        self.update(id, |run| {
            run.phase = ProjectHandoffPhase::Failed;
            run.current_activity = None;
            run.error = Some(message.to_owned());
            run.completed_at = Some(Utc::now().to_rfc3339());
        })
        .await
    }

    async fn update(
        &self,
        id: &str,
        update: impl FnOnce(&mut ProjectHandoffSnapshot),
    ) -> Result<()> {
        {
            let mut runs = self.state.write().await;
            let run = runs
                .iter_mut()
                .find(|run| run.id == id)
                .with_context(|| format!("unknown project handoff {id}"))?;
            update(run);
        }
        self.persist().await
    }

    async fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("create project handoff directory {}", parent.display())
            })?;
        }
        let content = serde_json::to_vec_pretty(&ProjectHandoffDocument {
            runs: self.state.read().await.clone(),
        })
        .context("encode project handoff journal")?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write project handoff journal {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace project handoff journal {}", self.path.display()))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_worker(
    mut receiver: ProjectHandoffReceiver,
    queue: Arc<ProjectHandoffQueue>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    assets: Arc<AssetStore>,
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
                Arc::clone(&usage),
                request.clone(),
            )
            .await
            {
                error!(handoff_id = %request.id, error = %worker_error, "Codex project handoff failed");
                let _ = queue.fail(&request.id, &worker_error.to_string()).await;
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_one(
    queue: Arc<ProjectHandoffQueue>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    profile: Arc<ProfileStore>,
    continuity: Arc<ContinuityHost>,
    assets: Arc<AssetStore>,
    usage: Arc<UsageStore>,
    request: ProjectHandoffRequest,
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
        .start_project_handoff(&request, &local_images, &compute, &profile, runtime_tx)
        .await;
    activity_task
        .await
        .context("join Codex project handoff activity relay")?;
    let outcome = outcome?;
    usage.record_all(&outcome.invocations).await?;
    queue
        .complete(&request.id, outcome.task_id, outcome.task_title)
        .await
}

fn validate_request(
    instruction: &str,
    reason: &str,
    image_revision_ids: &[String],
    lane: ComputeLane,
) -> Result<()> {
    if instruction.is_empty() || instruction.chars().count() > MAX_INSTRUCTION_CHARS {
        anyhow::bail!(
            "project handoff instruction must contain 1-{MAX_INSTRUCTION_CHARS} characters"
        );
    }
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        anyhow::bail!("project handoff reason must contain 1-{MAX_REASON_CHARS} characters");
    }
    if image_revision_ids.len() > MAX_TASK_IMAGES {
        anyhow::bail!("a project handoff can receive at most {MAX_TASK_IMAGES} images");
    }
    let mut unique_images = std::collections::HashSet::new();
    if image_revision_ids.iter().any(|revision_id| {
        revision_id.trim().is_empty()
            || revision_id.len() > 128
            || !unique_images.insert(revision_id.as_str())
    }) {
        anyhow::bail!("project handoff image Revision IDs must be non-empty and unique");
    }
    if !matches!(lane, ComputeLane::Investigate | ComputeLane::Critical) {
        anyhow::bail!("project handoff requires investigate or critical compute");
    }
    Ok(())
}

fn trim_runs(runs: &mut Vec<ProjectHandoffSnapshot>) {
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
        std::env::temp_dir().join(format!("symbiont-project-handoffs-{name}-{nonce}.json"))
    }

    fn project() -> BoundProject {
        BoundProject {
            id: "project-symbiont-d".to_owned(),
            title: "symbiont-d".to_owned(),
            cwd: "/tmp/symbiont-d".to_owned(),
            selected_at: Utc::now().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn handoff_requires_an_enabled_project_lease() {
        let (queue, _receiver) = ProjectHandoffQueue::open(test_path("gate")).await.unwrap();
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
                Some(
                    ProjectLease::new(project(), ProjectLeaseScope::OneShot)
                        .for_turn("user-revision-1"),
                ),
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
        assert_eq!(run.phase, ProjectHandoffPhase::Queued);
        assert_eq!(run.project.id, "project-symbiont-d");
        assert_eq!(run.image_revision_ids, vec!["rev_image"]);
        assert_eq!(run.project_scope, Some(ProjectLeaseScope::OneShot));
        assert_eq!(run.source_revision_id.as_deref(), Some("user-revision-1"));
        assert!(run.codex_task_id.is_none());
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
        let (queue, _receiver) = ProjectHandoffQueue::open(path.clone()).await.unwrap();
        queue
            .configure(
                true,
                Some(
                    ProjectLease::new(project(), ProjectLeaseScope::Topic)
                        .for_turn("user-revision-2"),
                ),
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

        let (reopened, _receiver) = ProjectHandoffQueue::open(path).await.unwrap();
        assert_eq!(
            reopened.snapshot().await[0].phase,
            ProjectHandoffPhase::Interrupted
        );
    }
}
