#![recursion_limit = "256"]

mod ambient_api;
mod asset;
mod audio_transcription;
mod autonomy;
mod bridge;
mod codex;
mod compute;
mod compute_policy;
mod context_maintenance;
mod continuation;
mod continuity;
mod conversation;
mod conversation_projection;
mod curiosity;
mod diagnostics;
mod drive_input;
mod exploration;
mod external_digest;
mod external_markdown;
mod identity;
mod input_roles;
mod luna_input;
mod mail_input;
mod maintenance;
mod memory;
mod outreach;
mod pcp_connection;
mod pcp_index;
mod permission;
mod profile;
mod reconciliation;
mod reflection;
mod rollover;
mod runtime_log;
mod secrets;
mod sensing;
mod signals;
mod startup;
mod symbiont_context;
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

use ambient_api::{AmbientScout, AmbientTopologyStore};
use anyhow::{Context, Result};
use asset::AssetStore;
use audio_transcription::AudioTranscriptionStore;
use autonomy::AutonomyStore;
use bridge::CodexBridge;
use codex::{CodexClient, CodexConfig, CodexTaskSources};
use compute::ComputeStore;
use compute_policy::ComputePolicyStore;
use continuation::ContinuationQueue;
use continuity::ContinuityHost;
use conversation::ConversationCoordinator;
use curiosity::CuriosityStore;
use drive_input::DriveInputStore;
use exploration::{
    ExplorationAttemptStore, ExplorationHandle, ExplorationIntentQueue, ManualExplorationStore,
};
use identity::IdentityStore;
use input_roles::InputRoleStore;
use luna_input::LunaInput;
use mail_input::MailInputStore;
use memory::MemoryStore;
use pcp_index::PcpIndex;
use permission::PermissionBroker;
use profile::ProfileStore;
use reconciliation::{ReconciliationDependencies, ReconciliationHandle, ReconciliationStore};
use reflection::{ReflectionHandle, ReflectionStore};
use sensing::SensingStore;
use signals::SignalStore;
use symbiont_context::SymbiontContextStore;
use tokio::{
    sync::Mutex,
    time::{Duration, sleep},
};
use tracing::info;
use usage::UsageStore;
use web::AppState;
use web_fetch::WebFetcher;

const DEFAULT_BIND: &str = "127.0.0.1:4317";

#[tokio::main]
async fn main() -> Result<()> {
    let workspace = env::current_dir().context("resolve the current workspace")?;
    runtime_log::init(resolve_data_path(
        &workspace,
        "SYMBIONT_RUNTIME_LOG_PATH",
        "logs/runtime.log",
    ))?;
    tracing::info!(
        target: runtime_log::TARGET,
        event = "service_starting",
        "symbiont-d is starting"
    );
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
    let identity = Arc::new(
        IdentityStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_IDENTITY_PATH",
            "identity.toml",
        ))
        .await?,
    );
    let pcp = pcp_connection::open(&workspace).await?;
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
    let pcp_index = Arc::new(PcpIndex::new(
        Arc::clone(&continuity),
        Arc::clone(&reflection_store),
    ));
    let index_calibration = pcp_index
        .sync_all()
        .await
        .context("calibrate the PCP model-written index")?;
    info!(
        episode_pages = index_calibration.episode_pages,
        created_pages = index_calibration.created_pages,
        revised_pages = index_calibration.revised_pages,
        unchanged_pages = index_calibration.unchanged_pages,
        "PCP model-written index is calibrated"
    );
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
    let (continuations, continuation_receiver) = ContinuationQueue::new();
    let continuations = Arc::new(continuations);
    let (exploration_intents, exploration_intent_receiver) =
        ExplorationIntentQueue::open(resolve_data_path(
            &workspace,
            "SYMBIONT_EXPLORATION_INTENTS_PATH",
            "exploration-intents.json",
        ))
        .await?;
    let exploration_intents = Arc::new(exploration_intents);
    let manual_exploration_runs = Arc::new(
        ManualExplorationStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_EXPLORATION_RECEIPTS_PATH",
            "exploration-receipts.json",
        ))
        .await?,
    );
    let exploration_attempts = Arc::new(
        ExplorationAttemptStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_EXPLORATION_ATTEMPTS_PATH",
            "exploration-attempts.json",
        ))
        .await?,
    );

    let codex_config = CodexConfig {
        binary: env::var("CODEX_BIN").unwrap_or_else(|_| "codex".to_owned()),
        workspace: workspace.clone(),
    };
    let bind: SocketAddr = env::var("SYMBIONT_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
        .parse()
        .context("parse SYMBIONT_BIND")?;
    let shell = startup::StartupShell::new();
    let listener = startup::bind(bind)
        .await
        .with_context(|| format!("bind the local interface at {bind}"))?;
    let server_shell = shell.clone();
    let server = tokio::spawn(async move { startup::serve(listener, server_shell).await });
    info!("symbiont-d is listening at http://{bind} (Codex may still be reconnecting)");

    let mut retry = shell.retry_subscriber();
    let codex = loop {
        let startup = CodexClient::start(
            codex_config.clone(),
            Arc::clone(&continuity),
            Arc::clone(&profile),
            Arc::clone(&context),
            Arc::clone(&curiosity),
            Arc::clone(&reflection_store),
            Arc::clone(&compute_policies),
            Arc::clone(&permissions),
            Arc::clone(&web_fetcher),
            Arc::clone(&continuations),
            Arc::clone(&exploration_intents),
        );
        match tokio::select! {
            result = startup => result,
            changed = retry.changed() => {
                if changed.is_ok() {
                    shell.set_status("正在立即重新连接 Codex…").await;
                    continue;
                }
                Err(anyhow::anyhow!("Codex retry control closed"))
            }
        } {
            Ok(client) => break client,
            Err(error) => {
                let message = format!("Codex 暂时不可用，正在后台重连：{error}");
                tracing::warn!(target: runtime_log::TARGET, event = "codex_startup_degraded", %error, "serving startup shell while Codex reconnects");
                shell.set_status(message).await;
                tokio::select! {
                    _ = sleep(Duration::from_secs(5)) => {}
                    changed = retry.changed() => {
                        if changed.is_ok() {
                            shell.set_status("正在立即重新连接 Codex…").await;
                        }
                    }
                }
            }
        }
    };

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
    let sensing = Arc::new(
        SensingStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_SENSING_CANDIDATES_PATH",
            "sensing-candidates.json",
        ))
        .await?,
    );
    let ambient_provider = Arc::new(
        AmbientTopologyStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_AMBIENT_PROVIDER_PATH",
            "ambient-provider.toml",
        ))
        .await?,
    );
    let ambient_scout = Arc::new(AmbientScout::new(Arc::clone(&ambient_provider))?);
    let luna_input = Arc::new(LunaInput::new(Arc::clone(&ambient_provider)));
    let mail_input = Arc::new(
        MailInputStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_MAIL_INPUT_PATH",
            "mail-input.toml",
        ))
        .await?,
    );
    let drive_input = Arc::new(
        DriveInputStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_DRIVE_INPUT_PATH",
            "drive-input.toml",
        ))
        .await?,
    );
    let audio_transcription = Arc::new(
        AudioTranscriptionStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_AUDIO_TRANSCRIPTION_PATH",
            "infer-runtime.toml",
        ))
        .await?,
    );
    let signals = Arc::new(
        SignalStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_INPUT_SIGNALS_PATH",
            "input-signals.json",
        ))
        .await?,
    );
    let input_roles = Arc::new(
        InputRoleStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_INPUT_ROLES_PATH",
            "input-roles.toml",
        ))
        .await?,
    );
    let reconciliation_store = Arc::new(
        ReconciliationStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_RECONCILIATION_PATH",
            "reconciliation.json",
        ))
        .await?,
    );
    let rate_limits = codex.rate_limits();
    let codex = Arc::new(Mutex::new(codex));
    let task_sources = Arc::new(CodexTaskSources::new(codex_config));
    let bridge = Arc::new(
        CodexBridge::open(
            resolve_data_path(
                &workspace,
                "SYMBIONT_CODEX_BRIDGE_PATH",
                "codex-bridge.toml",
            ),
            Arc::clone(&task_sources),
            Arc::clone(&continuity),
            Arc::clone(&profile),
            Arc::clone(&context),
            Arc::clone(&curiosity),
            Arc::clone(&reflection_store),
            Arc::clone(&assets),
        )
        .await?,
    );
    let conversation = ConversationCoordinator::new();
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
        Arc::clone(&ambient_scout),
        Arc::clone(&luna_input),
        Arc::clone(&drive_input),
        Arc::clone(&mail_input),
        Arc::clone(&sensing),
        Arc::clone(&signals),
        conversation.clone(),
        Arc::clone(&exploration_intents),
        Arc::clone(&manual_exploration_runs),
        Arc::clone(&exploration_attempts),
        exploration_intent_receiver,
    )
    .await;
    let reflection = ReflectionHandle::start(
        Arc::clone(&reflection_store),
        Arc::clone(&pcp_index),
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
    let reconciliation = ReconciliationHandle::start(
        reconciliation_store,
        ReconciliationDependencies {
            autonomy: Arc::clone(&autonomy),
            profile: Arc::clone(&profile),
            codex: Arc::clone(&codex),
            compute: Arc::clone(&compute),
            continuity: Arc::clone(&continuity),
            reflection: Arc::clone(&reflection_store),
            usage: Arc::clone(&usage),
            conversation: conversation.clone(),
        },
    );
    maintenance::start(
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&continuity),
        Arc::clone(&usage),
        conversation.clone(),
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
        conversation.clone(),
        resolve_data_path(
            &workspace,
            "SYMBIONT_CONTEXT_MAINTENANCE_PATH",
            "context-maintenance.json",
        ),
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
        identity,
        profile,
        context,
        curiosity,
        autonomy,
        codex,
        compute,
        ambient_provider,
        drive_input,
        mail_input,
        audio_transcription,
        compute_policies,
        usage,
        rate_limits,
        exploration,
        signals,
        input_roles,
        reflection,
        reconciliation,
        pcp_index,
        conversation,
        bridge,
        permissions,
        continuations,
    );
    shell.set_ready(state).await;
    tracing::info!(
        target: runtime_log::TARGET,
        event = "service_ready",
        bind = %bind,
        "symbiont-d is ready"
    );
    server.await.context("join startup shell")??;
    Ok(())
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
