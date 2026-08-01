mod asset;
mod autonomy;
mod bridge;
mod codex;
mod compute;
mod compute_policy;
mod context_maintenance;
mod continuation;
mod continuity;
mod conversation;
mod curiosity;
mod diagnostics;
mod exploration;
mod maintenance;
mod memory;
mod permission;
mod profile;
mod reflection;
mod rollover;
mod symbiont_context;
mod task_execution;
mod topics;
mod usage;
mod web;
mod web_fetch;
mod working_context;

use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use asset::AssetStore;
use autonomy::AutonomyStore;
use bridge::CodexBridge;
use codex::{CodexClient, CodexConfig};
use compute::ComputeStore;
use compute_policy::ComputePolicyStore;
use continuation::ContinuationQueue;
use continuity::ContinuityHost;
use conversation::ConversationCoordinator;
use curiosity::CuriosityStore;
use exploration::ExplorationHandle;
use memory::MemoryStore;
use pcp_sqlite::SqlitePcpStore;
use permission::PermissionBroker;
use profile::ProfileStore;
use reflection::{ReflectionHandle, ReflectionStore};
use symbiont_context::SymbiontContextStore;
use task_execution::TaskExecutionQueue;
use tokio::{net::TcpListener, sync::Mutex};
use tracing::info;
use tracing_subscriber::EnvFilter;
use usage::UsageStore;
use web::AppState;
use web_fetch::WebFetcher;

const DEFAULT_BIND: &str = "127.0.0.1:4317";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("symbiont_d=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let workspace = env::current_dir().context("resolve the current workspace")?;
    let memory_path = resolve_memory_path(&workspace);
    let memory = Arc::new(MemoryStore::open(memory_path).await?);
    let profile = Arc::new(
        ProfileStore::open(
            resolve_data_path(&workspace, "SYMBIONT_PROFILE_PATH", "profile.toml"),
            resolve_data_path(&workspace, "SYMBIONT_ORIENTATION_PATH", "orientation.md"),
        )
        .await?,
    );
    let autonomy = Arc::new(
        AutonomyStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_AUTONOMY_PATH",
            "autonomy.toml",
        ))
        .await?,
    );
    let assets = Arc::new(
        AssetStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_ASSET_PATH",
            "assets",
        ))
        .await?,
    );
    let pcp = Arc::new(
        SqlitePcpStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_PCP_PATH",
            "context.sqlite3",
        ))
        .await?,
    );
    let continuity = Arc::new(ContinuityHost::open(pcp).await?);
    let context = Arc::new(SymbiontContextStore::new(Arc::clone(&continuity)));
    let curiosity = Arc::new(CuriosityStore::new(Arc::clone(&continuity)));
    let migration = continuity
        .migrate_legacy(&memory, &profile.snapshot().await)
        .await
        .context("migrate legacy symbiont context into PCP")?;
    info!(
        migrated_messages = migration.migrated_messages,
        orientation_ready = migration.orientation.is_some(),
        "PCP continuity store is ready"
    );
    let reflection_store = Arc::new(
        ReflectionStore::open(
            resolve_data_path(&workspace, "SYMBIONT_REFLECTION_PATH", "reflection.sqlite3"),
            resolve_data_path(
                &workspace,
                "SYMBIONT_REFLECTION_CONFIG_PATH",
                "reflection.toml",
            ),
        )
        .await?,
    );
    reflection_store
        .backfill_messages(&continuity.recent_messages(100).await?)
        .await
        .context("backfill recent conversation into Reflection")?;
    let compute_policies = Arc::new(
        ComputePolicyStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_COMPUTE_POLICY_PATH",
            "compute-policies.toml",
        ))
        .await?,
    );
    let permissions = Arc::new(PermissionBroker::new());
    let web_fetcher = Arc::new(WebFetcher::new(Arc::clone(&permissions))?);
    let (task_execution, task_execution_receiver) = TaskExecutionQueue::open(resolve_data_path(
        &workspace,
        "SYMBIONT_TASK_EXECUTION_PATH",
        "task-runs.json",
    ))
    .await?;
    let task_execution = Arc::new(task_execution);
    let (continuations, continuation_receiver) = ContinuationQueue::new();
    let continuations = Arc::new(continuations);

    let codex = CodexClient::start(
        CodexConfig {
            binary: env::var("CODEX_BIN").unwrap_or_else(|_| "codex".to_owned()),
            workspace: workspace.clone(),
        },
        Arc::clone(&continuity),
        Arc::clone(&profile),
        Arc::clone(&context),
        Arc::clone(&curiosity),
        Arc::clone(&reflection_store),
        Arc::clone(&compute_policies),
        Arc::clone(&permissions),
        Arc::clone(&web_fetcher),
        Arc::clone(&task_execution),
        Arc::clone(&continuations),
    )
    .await
    .context("start the Codex app-server session")?;

    let compute = Arc::new(
        ComputeStore::open(
            resolve_data_path(&workspace, "SYMBIONT_COMPUTE_PATH", "compute.toml"),
            codex.models().to_vec(),
        )
        .await?,
    );
    let usage = Arc::new(
        UsageStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_USAGE_PATH",
            "symbiont.sqlite3",
        ))
        .await?,
    );
    let rate_limits = codex.rate_limits();
    let codex = Arc::new(Mutex::new(codex));
    let bridge = Arc::new(
        CodexBridge::open(
            resolve_data_path(
                &workspace,
                "SYMBIONT_CODEX_BRIDGE_PATH",
                "codex-bridge.toml",
            ),
            Arc::clone(&codex),
            Arc::clone(&continuity),
            Arc::clone(&profile),
            Arc::clone(&context),
            Arc::clone(&curiosity),
            Arc::clone(&reflection_store),
            Arc::clone(&task_execution),
            Arc::clone(&assets),
        )
        .await?,
    );
    let exploration = ExplorationHandle::start(
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&continuity),
        Arc::clone(&context),
        Arc::clone(&curiosity),
        Arc::clone(&reflection_store),
        Arc::clone(&usage),
    );
    let conversation = ConversationCoordinator::new();
    let reflection = ReflectionHandle::start(
        Arc::clone(&reflection_store),
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&continuity),
        Arc::clone(&context),
        Arc::clone(&curiosity),
        Arc::clone(&usage),
        conversation.clone(),
        exploration.clone(),
    );
    task_execution::start_worker(
        task_execution_receiver,
        Arc::clone(&task_execution),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&profile),
        Arc::clone(&continuity),
        Arc::clone(&assets),
        reflection.clone(),
        Arc::clone(&usage),
    );
    maintenance::start(
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&continuity),
        Arc::clone(&usage),
    );
    context_maintenance::start(
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&continuity),
        Arc::clone(&context),
        Arc::clone(&reflection_store),
        Arc::clone(&usage),
    );
    continuation::start_worker(
        continuation_receiver,
        Arc::clone(&continuations),
        conversation.clone(),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&profile),
        Arc::clone(&continuity),
        Arc::clone(&reflection_store),
        Arc::clone(&usage),
    );
    let state = AppState::new(
        continuity,
        assets,
        profile,
        context,
        curiosity,
        autonomy,
        codex,
        compute,
        compute_policies,
        usage,
        rate_limits,
        exploration,
        reflection,
        conversation,
        bridge,
        permissions,
        task_execution,
        continuations,
    );
    let app = web::router(state);
    let bind: SocketAddr = env::var("SYMBIONT_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
        .parse()
        .context("parse SYMBIONT_BIND")?;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind the local interface at {bind}"))?;

    info!("symbiont-d is listening at http://{bind}");
    axum::serve(listener, app).await.context("serve symbiont-d")
}

fn resolve_memory_path(workspace: &Path) -> PathBuf {
    env::var_os("SYMBIONT_MEMORY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("data/memory.md"))
}

fn resolve_data_path(workspace: &Path, variable: &str, filename: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("data").join(filename))
}
