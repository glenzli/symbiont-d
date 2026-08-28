#![recursion_limit = "256"]

mod ambient_api;
mod asset;
mod attacker;
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
mod ephemeral_chat;
mod ephemeral_session;
mod exploration;
mod external_digest;
mod external_markdown;
mod identity;
mod infer_runtime;
mod inference;
mod input_roles;
mod luna_input;
mod mail_input;
mod maintenance;
mod memory;
mod outreach;
mod pcp_connection;
mod pcp_index;
mod pcp_migration;
mod permission;
mod profile;
mod reconciliation;
mod reflection;
mod rollover;
mod runtime_log;
mod secrets;
mod sensing;
mod signal_retention;
mod signals;
mod startup;
mod symbiont_context;
mod symbiont_state;
mod topics;
mod transcript;
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
use attacker::AttackerHandle;
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
use ephemeral_chat::EphemeralChatService;
use exploration::{
    ExplorationAttemptStore, ExplorationHandle, ExplorationIntentQueue, ManualExplorationStore,
};
use identity::IdentityStore;
use infer_runtime::InferRuntimeAccess;
use inference::InferenceExecutor;
use input_roles::InputRoleStore;
use luna_input::LunaInput;
use mail_input::MailInputStore;
use pcp_index::PcpIndex;
use permission::PermissionBroker;
use profile::ProfileStore;
use reconciliation::{ReconciliationDependencies, ReconciliationHandle, ReconciliationStore};
use reflection::{ReflectionHandle, ReflectionStore};
use sensing::SensingStore;
use signal_retention::{SignalRetentionStore, start_cleanup as start_signal_cleanup};
use signals::SignalStore;
use symbiont_context::SymbiontContextStore;
use symbiont_state::SymbiontStateStore;
use tokio::{
    sync::Mutex,
    time::{Duration, sleep},
};
use tracing::info;
use transcript::TranscriptStore;
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
    let (transcript, transcript_restore) = TranscriptStore::open(
        resolve_data_path(&workspace, "SYMBIONT_TRANSCRIPT_PATH", "transcript.sqlite3"),
        default_legacy_snapshot_source(),
    )
    .await?;
    if transcript_restore.imported_messages > 0 {
        info!(
            imported_messages = transcript_restore.imported_messages,
            source_snapshot = ?transcript_restore.source_snapshot,
            "restored archived chat transcript into local storage"
        );
    }
    let transcript = Arc::new(transcript);
    let (symbiont_state, state_restore) = SymbiontStateStore::open(
        resolve_data_path(&workspace, "SYMBIONT_STATE_PATH", "symbiont-state.sqlite3"),
        default_legacy_snapshot_source(),
    )
    .await?;
    if state_restore.imported_records > 0
        || state_restore.imported_relations > 0
        || state_restore.imported_provenance > 0
        || state_restore.imported_context_documents > 0
    {
        info!(
            imported_records = state_restore.imported_records,
            imported_relations = state_restore.imported_relations,
            imported_provenance = state_restore.imported_provenance,
            imported_context_documents = state_restore.imported_context_documents,
            source_snapshot = ?state_restore.source_snapshot,
            "restored archived Symbiont state into local storage"
        );
    }
    let symbiont_state = Arc::new(symbiont_state);
    let pcp = pcp_connection::open(&workspace).await?;
    let continuity = Arc::new(
        ContinuityHost::open_at(
            pcp,
            Arc::clone(&transcript),
            resolve_data_path(
                &workspace,
                "SYMBIONT_PCP_SOURCE_SEQUENCE_PATH",
                "pcp-source-sequence.json",
            ),
        )
        .await?,
    );
    let context = Arc::new(SymbiontContextStore::from_state(Arc::clone(
        &symbiont_state,
    )));
    let curiosity = Arc::new(CuriosityStore::from_state(
        Arc::clone(&continuity),
        Arc::clone(&symbiont_state),
    ));
    info!(
        "local chat transcript and PCP recall client are ready; semantic maintenance is disabled"
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
    info!("PCP tenant semantic index is disabled; no historical projection will be rebuilt");
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
    let mut codex = loop {
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
    let infer_runtime = Arc::new(
        InferRuntimeAccess::open(resolve_data_path(
            &workspace,
            "SYMBIONT_INFER_RUNTIME_SECRETS_PATH",
            "infer-runtime-secrets.toml",
        ))
        .await?,
    );
    let audio_transcription = Arc::new(
        AudioTranscriptionStore::open(
            resolve_data_path(
                &workspace,
                "SYMBIONT_AUDIO_TRANSCRIPTION_PATH",
                "infer-runtime.toml",
            ),
            Arc::clone(&infer_runtime),
        )
        .await?,
    );
    let inference = Arc::new(InferenceExecutor::new(infer_runtime));
    let signals = Arc::new(
        SignalStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_INPUT_SIGNALS_PATH",
            "input-signals.json",
        ))
        .await?,
    );
    let signal_retention = Arc::new(
        SignalRetentionStore::open(resolve_data_path(
            &workspace,
            "SYMBIONT_SIGNAL_RETENTION_PATH",
            "input-signal-retention.toml",
        ))
        .await?,
    );
    start_signal_cleanup(Arc::clone(&signals), Arc::clone(&signal_retention));
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
    if env::var_os("SYMBIONT_RUN_PCP_TRANSCRIPT_MIGRATION").is_some() {
        let report = pcp_migration::migrate_transcript(
            &mut codex,
            Arc::clone(&transcript),
            Arc::clone(&continuity),
            Arc::clone(&compute),
            Arc::clone(&profile),
        )
        .await
        .context("run model-judged PCP transcript migration")?;
        info!(
            pcp_identity_id = report.pcp_identity_id,
            through_sequence = report.through_sequence,
            batches_completed = report.batches_completed,
            messages_assessed = report.messages_assessed,
            records_written = report.records_written,
            "PCP transcript migration is complete for this Store identity"
        );
    }
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
    let ephemeral_chat = Arc::new(EphemeralChatService::new(
        Arc::clone(&bridge),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&profile),
        Arc::clone(&usage),
    )?);
    let conversation = ConversationCoordinator::new();
    let exploration = ExplorationHandle::start(
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&inference),
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
    let attacker = AttackerHandle::start(
        resolve_data_path(&workspace, "SYMBIONT_ATTACKER_PATH", "attacker.json"),
        Arc::clone(&autonomy),
        Arc::clone(&profile),
        Arc::clone(&codex),
        Arc::clone(&compute),
        Arc::clone(&signals),
        Arc::clone(&usage),
        Arc::clone(&continuity),
        conversation.clone(),
    )
    .await?;
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
            inference: Arc::clone(&inference),
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
        Arc::clone(&inference),
        ambient_provider,
        drive_input,
        mail_input,
        audio_transcription,
        compute_policies,
        usage,
        rate_limits,
        exploration,
        attacker,
        signals,
        signal_retention,
        input_roles,
        reflection,
        reconciliation,
        pcp_index,
        conversation,
        bridge,
        ephemeral_chat,
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

fn resolve_data_path(workspace: &Path, variable: &str, filename: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("data").join(filename))
}

/// The last pre-semantic PCP snapshot is a one-time recovery source only.
/// New transcript and Symbiont state are written locally; neither is rebuilt
/// from live PCP.
fn default_legacy_snapshot_source() -> Option<PathBuf> {
    env::var_os("SYMBIONT_TRANSCRIPT_IMPORT_SOURCE")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join("Library/Application Support/PCP/data")
                    .join("context-v0.8-pre-semantic-20260815.sqlite3")
            })
        })
}
