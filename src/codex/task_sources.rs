use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio::sync::{Mutex, MutexGuard, RwLock};

use super::{
    CodexConfig, CodexTaskDetail, CodexTaskSummary, task_source_client::CodexTaskSourceClient,
};

const TASK_LIST_TTL: Duration = Duration::from_secs(90);
const TASK_DETAIL_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct Cached<T> {
    value: T,
    loaded_at: Instant,
}

impl<T> Cached<T> {
    fn fresh(&self, ttl: Duration) -> bool {
        self.loaded_at.elapsed() < ttl
    }
}

#[derive(Default)]
struct CacheState {
    tasks: Option<Cached<Vec<CodexTaskSummary>>>,
    details: HashMap<String, Cached<CodexTaskDetail>>,
}

/// Read-only, short-lived projection of interactive Codex tasks for the composer.
///
/// The cache never writes to Codex or persists task content. Its refresh gate avoids
/// duplicating slow app-server requests when the picker is opened while warming.
pub struct CodexTaskSources {
    config: CodexConfig,
    client: Mutex<Option<CodexTaskSourceClient>>,
    cache: RwLock<CacheState>,
    refresh: Mutex<()>,
}

impl CodexTaskSources {
    pub fn new(config: CodexConfig) -> Self {
        Self {
            config,
            client: Mutex::new(None),
            cache: RwLock::new(CacheState::default()),
            refresh: Mutex::new(()),
        }
    }

    pub async fn list(&self, refresh: bool) -> Result<Vec<CodexTaskSummary>> {
        if !refresh {
            if let Some(tasks) = self.cached_tasks().await {
                return Ok(tasks);
            }
        }

        let _refresh = self.refresh.lock().await;
        if !refresh {
            if let Some(tasks) = self.cached_tasks().await {
                return Ok(tasks);
            }
        }
        let mut client = self.connected_client().await?;
        let tasks = client
            .as_mut()
            .expect("task source client was just initialized")
            .list_tasks(30)
            .await;
        let tasks = match tasks {
            Ok(tasks) => tasks,
            Err(error) => {
                *client = None;
                return Err(error);
            }
        };
        self.cache.write().await.tasks = Some(Cached {
            value: tasks.clone(),
            loaded_at: Instant::now(),
        });
        Ok(tasks)
    }

    pub async fn read(&self, thread_id: &str) -> Result<CodexTaskDetail> {
        if let Some(detail) = self.cached_detail(thread_id).await {
            return Ok(detail);
        }

        let _refresh = self.refresh.lock().await;
        if let Some(detail) = self.cached_detail(thread_id).await {
            return Ok(detail);
        }
        let mut client = self.connected_client().await?;
        let detail = client
            .as_mut()
            .expect("task source client was just initialized")
            .read_task(thread_id)
            .await;
        let detail = match detail {
            Ok(detail) => detail,
            Err(error) => {
                *client = None;
                return Err(error);
            }
        };
        self.cache.write().await.details.insert(
            thread_id.to_owned(),
            Cached {
                value: detail.clone(),
                loaded_at: Instant::now(),
            },
        );
        Ok(detail)
    }

    async fn cached_tasks(&self) -> Option<Vec<CodexTaskSummary>> {
        self.cache
            .read()
            .await
            .tasks
            .as_ref()
            .filter(|cached| cached.fresh(TASK_LIST_TTL))
            .map(|cached| cached.value.clone())
    }

    async fn cached_detail(&self, thread_id: &str) -> Option<CodexTaskDetail> {
        self.cache
            .read()
            .await
            .details
            .get(thread_id)
            .filter(|cached| cached.fresh(TASK_DETAIL_TTL))
            .map(|cached| cached.value.clone())
    }

    async fn connected_client(&self) -> Result<MutexGuard<'_, Option<CodexTaskSourceClient>>> {
        let mut client = self.client.lock().await;
        if client.is_none() {
            *client = Some(CodexTaskSourceClient::start(self.config.clone()).await?);
        }
        Ok(client)
    }
}
