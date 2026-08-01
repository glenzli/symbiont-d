use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::warn;

use super::{
    ReconciliationMode, ReconciliationPhase, ReconciliationRuntime, ReconciliationSnapshot,
    ReconciliationStore, store::CompletedRun,
};
use crate::{
    autonomy::AutonomyStore,
    codex::{CodexClient, ReconciliationModelRequest, RuntimeEvent},
    compute::ComputeStore,
    continuity::ContinuityHost,
    conversation::ConversationCoordinator,
    exploration::today_started_at,
    profile::{ProfileStore, SetupStatus},
    reflection::ReflectionStore,
    usage::UsageStore,
};

#[derive(Clone)]
pub struct ReconciliationDependencies {
    pub autonomy: Arc<AutonomyStore>,
    pub profile: Arc<ProfileStore>,
    pub codex: Arc<Mutex<CodexClient>>,
    pub compute: Arc<ComputeStore>,
    pub continuity: Arc<ContinuityHost>,
    pub reflection: Arc<ReflectionStore>,
    pub usage: Arc<UsageStore>,
    pub conversation: ConversationCoordinator,
}

#[derive(Clone, Debug)]
enum ReconciliationTrigger {
    Preview,
    Apply {
        preview_run_id: String,
        bypass_token_limit: bool,
    },
}

#[derive(Clone)]
pub struct ReconciliationHandle {
    store: Arc<ReconciliationStore>,
    runtime: Arc<RwLock<ReconciliationRuntime>>,
    trigger: mpsc::Sender<ReconciliationTrigger>,
    queued_or_running: Arc<AtomicBool>,
}

impl ReconciliationHandle {
    pub fn start(
        store: Arc<ReconciliationStore>,
        dependencies: ReconciliationDependencies,
    ) -> Self {
        let runtime = Arc::new(RwLock::new(ReconciliationRuntime::default()));
        let queued_or_running = Arc::new(AtomicBool::new(false));
        let (trigger, trigger_rx) = mpsc::channel(8);
        tokio::spawn(run(
            Arc::clone(&store),
            Arc::clone(&runtime),
            dependencies,
            Arc::clone(&queued_or_running),
            trigger_rx,
        ));
        let handle = Self {
            store,
            runtime,
            trigger,
            queued_or_running,
        };
        handle
    }

    pub fn preview(&self) -> bool {
        self.enqueue(ReconciliationTrigger::Preview)
    }

    pub fn apply(&self, preview_run_id: String, bypass_token_limit: bool) -> bool {
        self.enqueue(ReconciliationTrigger::Apply {
            preview_run_id,
            bypass_token_limit,
        })
    }

    pub async fn runtime(&self) -> ReconciliationRuntime {
        let runtime = self.runtime.read().await.clone();
        hydrate_runtime(runtime, &self.store.recent_runs().await)
    }

    pub async fn snapshot(&self) -> ReconciliationSnapshot {
        let recent_runs = self.store.recent_runs().await;
        let runtime = hydrate_runtime(self.runtime.read().await.clone(), &recent_runs);
        ReconciliationSnapshot {
            runtime,
            latest_preview: self.store.latest_preview().await,
            recent_runs,
        }
    }

    fn enqueue(&self, trigger: ReconciliationTrigger) -> bool {
        if self
            .queued_or_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if self.trigger.try_send(trigger).is_err() {
            self.queued_or_running.store(false, Ordering::Release);
            return false;
        }
        true
    }
}

fn hydrate_runtime(
    mut runtime: ReconciliationRuntime,
    recent_runs: &[super::ReconciliationRun],
) -> ReconciliationRuntime {
    let Some(latest) = recent_runs.first() else {
        return runtime;
    };
    if runtime.last_run_at.is_none() {
        runtime.candidate_count = latest.candidate_count;
        runtime.last_run_at = latest
            .completed_at
            .clone()
            .or_else(|| Some(latest.started_at.clone()));
        runtime.last_summary = latest.summary.clone();
        runtime.last_error = latest.error.clone();
    }
    runtime
}

async fn run(
    store: Arc<ReconciliationStore>,
    runtime: Arc<RwLock<ReconciliationRuntime>>,
    dependencies: ReconciliationDependencies,
    queued_or_running: Arc<AtomicBool>,
    mut trigger_rx: mpsc::Receiver<ReconciliationTrigger>,
) {
    while let Some(trigger) = trigger_rx.recv().await {
        if let Err(error) = reconcile_once(&store, &runtime, &dependencies, trigger).await {
            warn!(%error, "durable memory Reconciliation failed");
            let mut current = runtime.write().await;
            current.phase = ReconciliationPhase::Error;
            current.last_error = Some(error.to_string());
            current.current_activity = None;
        }
        queued_or_running.store(false, Ordering::Release);
    }
}

async fn reconcile_once(
    store: &ReconciliationStore,
    runtime: &Arc<RwLock<ReconciliationRuntime>>,
    dependencies: &ReconciliationDependencies,
    trigger: ReconciliationTrigger,
) -> Result<()> {
    let profile = dependencies.profile.snapshot().await;
    if profile.status != SetupStatus::Ready {
        runtime.write().await.phase = ReconciliationPhase::NeedsSetup;
        return Ok(());
    }
    let bypass_token_limit = matches!(
        &trigger,
        ReconciliationTrigger::Apply {
            bypass_token_limit: true,
            ..
        }
    );
    if !bypass_token_limit && over_budget(dependencies).await? {
        runtime.write().await.phase = ReconciliationPhase::TokenLimit;
        return Ok(());
    }

    let inventory = build_inventory(dependencies).await?;
    {
        let mut current = runtime.write().await;
        current.candidate_count = inventory.candidate_count;
        current.last_error = None;
    }
    let (mode, trigger_name, preview) = match trigger {
        ReconciliationTrigger::Preview => (ReconciliationMode::Preview, "manual", None),
        ReconciliationTrigger::Apply { preview_run_id, .. } => {
            let preview = store
                .run(&preview_run_id)
                .await
                .context("the selected reconciliation preview no longer exists")?;
            if preview.mode != ReconciliationMode::Preview || preview.status != "completed" {
                anyhow::bail!("only a completed reconciliation preview can be applied");
            }
            if preview.proposals.is_empty() {
                anyhow::bail!("this reconciliation preview has no changes to apply");
            }
            let stale = preview
                .proposals
                .iter()
                .flat_map(|proposal| &proposal.revision_ids)
                .filter(|revision_id| !inventory.current_revision_ids.contains(*revision_id))
                .cloned()
                .collect::<Vec<_>>();
            if !stale.is_empty() {
                anyhow::bail!(
                    "the preview references non-current durable Revisions: {}",
                    stale.join(", ")
                );
            }
            if store.has_completed_apply(&preview_run_id).await {
                anyhow::bail!("this reconciliation preview has already been applied");
            }
            (ReconciliationMode::Apply, "manual", Some(preview))
        }
    };
    let run_id = store
        .start_run(
            mode,
            trigger_name,
            inventory.digest.clone(),
            inventory.candidate_count,
            preview.as_ref().map(|run| run.id.clone()),
        )
        .await?;
    {
        let mut current = runtime.write().await;
        current.phase = match mode {
            ReconciliationMode::Preview => ReconciliationPhase::Previewing,
            ReconciliationMode::Apply => ReconciliationPhase::Applying,
        };
        current.current_activity = Some(match mode {
            ReconciliationMode::Preview => "正在检查长期记忆结构".to_owned(),
            ReconciliationMode::Apply => "正在应用记忆整理建议".to_owned(),
        });
    }

    let compute = dependencies.compute.snapshot().await;
    let continuity_context = dependencies.continuity.context_seed(None).await;
    let proposals = preview
        .as_ref()
        .map(|run| run.proposals.as_slice())
        .unwrap_or_default();
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let runtime_for_events = Arc::clone(runtime);
    let event_drain = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if let RuntimeEvent::Activity { label, .. } = event {
                runtime_for_events.write().await.current_activity = Some(label);
            }
        }
    });
    let outcome = dependencies
        .codex
        .lock()
        .await
        .reconcile_memory(ReconciliationModelRequest {
            mode,
            run_id: &run_id,
            inventory_bundle: &inventory.bundle,
            proposals,
            compute: &compute,
            profile: &profile,
            continuity_context: &continuity_context,
            input_events: dependencies.conversation.subscribe_input(),
            events: events_tx,
        })
        .await;
    event_drain
        .await
        .context("join Reconciliation event drain")?;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            store.fail_run(&run_id, &error.to_string()).await?;
            return Err(error);
        }
    };
    dependencies.usage.record_all(&outcome.invocations).await?;
    if outcome.interrupted {
        store.interrupt_run(&run_id).await?;
        let mut current = runtime.write().await;
        current.phase = ReconciliationPhase::Idle;
        current.last_run_at = Some(now());
        current.last_summary = None;
        current.current_activity = None;
        return Ok(());
    }
    let trace_id = outcome.invocations.first().map(|item| item.id.clone());
    let model = outcome
        .invocations
        .last()
        .map(|item| item.model_display_name.clone());
    let total_tokens = outcome
        .invocations
        .iter()
        .map(|item| item.total_tokens)
        .sum();
    let summary = outcome.summary.clone();
    store
        .complete_run(
            &run_id,
            CompletedRun {
                summary: outcome.summary,
                proposals: outcome.proposals,
                actions: outcome.actions,
                trace_id,
                model,
                total_tokens,
            },
        )
        .await?;
    let mut current = runtime.write().await;
    current.phase = ReconciliationPhase::Idle;
    current.last_run_at = Some(now());
    current.last_summary = summary;
    current.last_error = None;
    current.current_activity = None;
    Ok(())
}

async fn over_budget(dependencies: &ReconciliationDependencies) -> Result<bool> {
    let headline = dependencies.usage.headline(&today_started_at()).await?;
    let autonomy_limit = dependencies.autonomy.snapshot().await.daily_token_limit;
    let reflection_limit = dependencies.reflection.config().await.daily_token_limit;
    Ok(
        (autonomy_limit > 0 && headline.autonomous_tokens_today >= autonomy_limit)
            || (reflection_limit > 0 && headline.reflection_tokens_today >= reflection_limit),
    )
}

struct InventoryBundle {
    digest: String,
    candidate_count: usize,
    bundle: String,
    current_revision_ids: HashSet<String>,
}

async fn build_inventory(dependencies: &ReconciliationDependencies) -> Result<InventoryBundle> {
    let pages = dependencies.continuity.durable_page_inventory().await?;
    let episodes = dependencies.reflection.episodes(20).await?;
    let page_index = pages
        .iter()
        .take(60)
        .map(|page| {
            let routing_text = page
                .summary
                .as_deref()
                .map(|summary| truncate(summary, 700))
                .unwrap_or_else(|| truncate(&page.snippet, 1_200));
            json!({
                "pageId": page.page_id,
                "revisionId": page.revision_id,
                "namespace": page.namespace,
                "kind": page.kind,
                "observedAt": page.observed_at,
                "contentChars": page.content_chars,
                "routingText": routing_text,
                "hasSummary": page.summary_revision_id.is_some(),
                "facets": page.facets,
                "relationTypes": page.relation_types,
            })
        })
        .collect::<Vec<Value>>();
    let value = json!({
        "durablePages": page_index,
        "topicEpisodes": episodes,
    });
    let identity = json!({
        "durablePageHeads": pages.iter().map(|page| json!({
            "pageId": page.page_id,
            "revisionId": page.revision_id,
        })).collect::<Vec<_>>(),
        "topicEpisodeHeads": value["topicEpisodes"].as_array().into_iter().flatten().map(|episode| json!({
            "id": episode["id"],
            "updatedAt": episode["updatedAt"],
            "state": episode["state"],
        })).collect::<Vec<_>>(),
    });
    let compact = serde_json::to_vec(&identity).context("encode memory inventory digest")?;
    let digest = format!("{:x}", Sha256::digest(&compact));
    Ok(InventoryBundle {
        digest,
        candidate_count: pages.len(),
        bundle: serde_json::to_string_pretty(&value).context("encode memory inventory prompt")?,
        current_revision_ids: pages.into_iter().map(|page| page.revision_id).collect(),
    })
}

fn truncate(value: &str, limit: usize) -> String {
    let mut value = value.chars().take(limit).collect::<String>();
    if value.chars().count() == limit {
        value.push('…');
    }
    value
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
