use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{RwLock, mpsc, watch},
    time::{sleep, timeout},
};
use tracing::{debug, warn};

use super::{
    approvals::{approval_response, automatic_server_request_response, permission_request},
    autonomous::{
        finding_from_invocations, review_prompt, scout_prompt, sensing_candidates_from_invocations,
        sensing_prompt,
    },
    interaction_output::{
        ChatDisposition, InteractiveDeltaGate, interaction_disposition_prompt,
        interpret_interactive_output,
    },
    prompts::{
        additional_context_value, ambient_sense_developer_instructions, context_fragments,
        context_maintenance_prompt, developer_instructions, interaction_reflection_prompt,
        memory_reconciliation_prompt, pcp_maintenance_developer_instructions,
        pcp_maintenance_worker_prompt, profile_review_prompt, summary_maintenance_prompt,
    },
    tool_dedup::{ToolCallPlan, TurnToolDeduplicator},
    tools::{EscalationRequest, SymbiontTools, tool_result},
    trace::{
        observable_history_item, observable_item_event, push_trace_event, timestamp_from_millis,
    },
};
use crate::{
    compute::{ComputeConfig, ComputeLane, LaneConfig, ModelInfo},
    continuation::ContinuationQueue,
    continuity::ContinuityHost,
    curiosity::CuriosityStore,
    diagnostics::{
        ContextFragment, ContextSnapshot, ExecutionTraceEvent, NativeThreadSnapshot, TraceEventKind,
    },
    exploration::ExplorationIntentQueue,
    memory::{MessageMetadata, MessageRunMetadata},
    outreach::{OutreachCandidate, PROPOSE_OUTREACH_TOOL},
    permission::PermissionBroker,
    profile::{ProfileSnapshot, ProfileStore},
    reconciliation::{
        ReconciliationAction, ReconciliationMode, ReconciliationModelOutcome,
        ReconciliationProposal,
    },
    reflection::ReflectionStore,
    rollover::{self, NativeThreadCursor, RolloverDecision, ThreadContextPressure},
    symbiont_context::SymbiontContextStore,
    usage::{InvocationRecord, ToolTraceStep},
    web_fetch::WebFetcher,
    working_context::WorkingContext,
};

const AUTONOMOUS_SILENT_MARKER: &str = "<symbiont-silent/>";
const AUTONOMOUS_SUPERSEDED_MARKER: &str = "<symbiont-superseded/>";
const MAINTENANCE_COMPLETE_MARKER: &str = "<symbiont-maintained/>";
const CONTEXT_MAINTENANCE_COMPLETE_MARKER: &str = "<symbiont-context-maintained/>";
const PROFILE_REVIEW_COMPLETE_MARKER: &str = "<symbiont-profile-reviewed/>";
const REFLECTION_COMPLETE_MARKER: &str = "<symbiont-reflected/>";
const RECONCILIATION_COMPLETE_MARKER: &str = "<symbiont-reconciled/>";
const PCP_MAINTENANCE_COMPLETE_MARKER: &str = "<symbiont-pcp-maintained/>";
const CONTINUATION_SILENT_MARKER: &str = "<symbiont-no-continuation/>";

#[derive(Clone)]
pub struct CodexConfig {
    pub binary: String,
    pub workspace: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    pub limit_id: Option<String>,
    pub plan_type: Option<String>,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<u64>,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RuntimeEvent {
    Activity {
        label: String,
        model: String,
        display_name: String,
        effort: String,
        lane: String,
    },
    Delta {
        text: String,
    },
    Reset,
}

pub struct ChatOutcome {
    pub text: String,
    pub disposition: ChatDisposition,
    pub generated_images: Vec<GeneratedImageOutput>,
    pub metadata: MessageMetadata,
    pub invocations: Vec<InvocationRecord>,
    pub context_revision_ids: Vec<String>,
    pub scheduled_follow_up_ids: Vec<String>,
    pub reserved_continuation_ids: Vec<String>,
    pub requested_exploration_ids: Vec<String>,
    pub interrupted: bool,
}

#[derive(Clone, Debug)]
pub struct GeneratedImageOutput {
    pub item_id: String,
    pub saved_path: PathBuf,
    pub revised_prompt: Option<String>,
}

pub struct ExplorationOutcome {
    pub outreach: Option<OutreachCandidate>,
    pub metadata: MessageMetadata,
    pub invocations: Vec<InvocationRecord>,
    pub context_revision_ids: Vec<String>,
    pub hunch_revisions: Vec<HunchRevisionRef>,
    pub superseded: bool,
    pub interrupted: bool,
}

pub struct SensingOutcome {
    pub candidates: Vec<crate::sensing::SensingCandidateDraft>,
    pub invocations: Vec<InvocationRecord>,
    pub interrupted: bool,
}

#[derive(Clone, Debug)]
pub struct HunchRevisionRef {
    pub page_id: String,
    pub revision_id: String,
}

pub struct MaintenanceOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub summarized: bool,
    pub model: Option<String>,
    pub interrupted: bool,
}

pub struct ContextMaintenanceOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub current_map_updated: bool,
    pub open_loops_updated: bool,
    pub interrupted: bool,
}

pub struct ProfileReviewOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub status: Option<String>,
    pub clarification_question: Option<String>,
    pub metadata: MessageMetadata,
    pub context_revision_ids: Vec<String>,
    pub interrupted: bool,
}

pub struct ReflectionOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub summary: Option<String>,
    pub actions: Vec<String>,
    pub metadata: MessageMetadata,
    pub outreach: Option<OutreachCandidate>,
    pub context_revision_ids: Vec<String>,
    pub interrupted: bool,
}

pub struct ChatInput {
    pub text: String,
    pub local_images: Vec<PathBuf>,
    pub current_revision_id: String,
    pub reply_to_revision_id: Option<String>,
    pub initial_lane: ComputeLane,
    pub input_events: watch::Receiver<u64>,
}

pub struct ReconciliationModelRequest<'a> {
    pub mode: ReconciliationMode,
    pub run_id: &'a str,
    pub inventory_bundle: &'a str,
    pub proposals: &'a [ReconciliationProposal],
    pub compute: &'a ComputeConfig,
    pub profile: &'a ProfileSnapshot,
    pub continuity_context: &'a str,
    pub input_events: watch::Receiver<u64>,
    pub events: mpsc::Sender<RuntimeEvent>,
}

pub struct PcpMaintenanceModelRequest<'a> {
    pub request: &'a pcp_runtime::MaintenanceWorkerRequest,
    pub compute: &'a ComputeConfig,
    pub profile: &'a ProfileSnapshot,
    pub input_events: watch::Receiver<u64>,
    pub events: mpsc::Sender<RuntimeEvent>,
}

pub struct PcpMaintenanceModelOutcome {
    pub response: pcp_runtime::MaintenanceWorkerResponse,
    pub invocations: Vec<InvocationRecord>,
    pub interrupted: bool,
}

#[derive(Clone, Debug, Default)]
struct TokenBreakdown {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

struct TurnOutcome {
    text: String,
    generated_images: Vec<GeneratedImageOutput>,
    invocation: InvocationRecord,
    escalation: Option<EscalationRequest>,
    interrupted: bool,
}

struct TurnOverrides {
    cwd: PathBuf,
    sandbox_policy: Value,
}

#[derive(Clone, Copy)]
enum BackgroundThread {
    AmbientSense,
    AutonomousScout,
    AutonomousReview,
    Maintenance,
    PcpMaintenance,
}

#[derive(Clone, Copy)]
enum ToolSurface {
    Full,
    AmbientSense,
    AutonomousScout,
    PcpMaintenance,
}

pub struct CodexClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    interactive_thread_id: String,
    ambient_sense_thread_id: String,
    autonomous_scout_thread_id: String,
    autonomous_review_thread_id: String,
    maintenance_thread_id: String,
    pcp_maintenance_thread_id: String,
    workspace: PathBuf,
    continuity: Arc<ContinuityHost>,
    interactive_cursor: NativeThreadCursor,
    tools: SymbiontTools,
    models: Vec<ModelInfo>,
    thread_usage: HashMap<String, TokenBreakdown>,
    thread_turns: HashMap<String, u64>,
    thread_compactions: HashMap<String, u64>,
    thread_context_pressure: HashMap<String, ThreadContextPressure>,
    rate_limits: Arc<RwLock<Option<RateLimitInfo>>>,
    permissions: Arc<PermissionBroker>,
    compute_policies: Arc<crate::compute_policy::ComputePolicyStore>,
}

impl CodexClient {
    pub async fn start(
        config: CodexConfig,
        continuity: Arc<ContinuityHost>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        compute_policies: Arc<crate::compute_policy::ComputePolicyStore>,
        permissions: Arc<PermissionBroker>,
        web_fetcher: Arc<WebFetcher>,
        continuations: Arc<ContinuationQueue>,
        exploration_intents: Arc<ExplorationIntentQueue>,
    ) -> Result<Self> {
        let mut last_error = None;
        for attempt in 1..=3 {
            match timeout(
                Duration::from_secs(20),
                Self::start_once(
                    config.clone(),
                    Arc::clone(&continuity),
                    Arc::clone(&profile),
                    Arc::clone(&context),
                    Arc::clone(&curiosity),
                    Arc::clone(&reflection),
                    Arc::clone(&compute_policies),
                    Arc::clone(&permissions),
                    Arc::clone(&web_fetcher),
                    Arc::clone(&continuations),
                    Arc::clone(&exploration_intents),
                ),
            )
            .await
            {
                Ok(Ok(client)) => return Ok(client),
                Ok(Err(error)) => {
                    warn!(attempt, %error, "Codex app-server startup attempt failed");
                    last_error = Some(error);
                }
                Err(_) => {
                    warn!(attempt, "Codex app-server startup attempt timed out");
                    last_error = Some(anyhow::anyhow!(
                        "Codex app-server startup timed out after 20 seconds"
                    ));
                }
            }
            sleep(Duration::from_millis(250)).await;
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Codex app-server did not start")))
    }

    async fn start_once(
        config: CodexConfig,
        continuity: Arc<ContinuityHost>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
        reflection: Arc<ReflectionStore>,
        compute_policies: Arc<crate::compute_policy::ComputePolicyStore>,
        permissions: Arc<PermissionBroker>,
        web_fetcher: Arc<WebFetcher>,
        continuations: Arc<ContinuationQueue>,
        exploration_intents: Arc<ExplorationIntentQueue>,
    ) -> Result<Self> {
        let mut child = Command::new(&config.binary)
            .arg("app-server")
            .arg("--stdio")
            .current_dir(&config.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {} app-server", config.binary))?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex app-server did not expose stdout")?;

        let mut client = Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 1,
            interactive_thread_id: String::new(),
            ambient_sense_thread_id: String::new(),
            autonomous_scout_thread_id: String::new(),
            autonomous_review_thread_id: String::new(),
            maintenance_thread_id: String::new(),
            pcp_maintenance_thread_id: String::new(),
            workspace: config.workspace.clone(),
            continuity: Arc::clone(&continuity),
            interactive_cursor: NativeThreadCursor::new(),
            tools: SymbiontTools::new(
                continuity,
                profile,
                context,
                curiosity,
                reflection,
                Arc::clone(&compute_policies),
                Some(web_fetcher),
                continuations,
                exploration_intents,
            ),
            models: Vec::new(),
            thread_usage: HashMap::new(),
            thread_turns: HashMap::new(),
            thread_compactions: HashMap::new(),
            thread_context_pressure: HashMap::new(),
            rate_limits: Arc::new(RwLock::new(None)),
            permissions,
            compute_policies,
        };
        client.initialize().await?;
        client.models = client.load_models().await?;
        client.refresh_rate_limits().await;
        client.interactive_thread_id = client
            .start_thread(&config.workspace, ToolSurface::Full)
            .await?;
        client.ambient_sense_thread_id = client
            .start_thread(&config.workspace, ToolSurface::AmbientSense)
            .await?;
        client.autonomous_scout_thread_id = client
            .start_thread(&config.workspace, ToolSurface::AutonomousScout)
            .await?;
        client.autonomous_review_thread_id = client
            .start_thread(&config.workspace, ToolSurface::Full)
            .await?;
        client.maintenance_thread_id = client
            .start_thread(&config.workspace, ToolSurface::Full)
            .await?;
        client.pcp_maintenance_thread_id = client
            .start_thread(&config.workspace, ToolSurface::PcpMaintenance)
            .await?;
        Ok(client)
    }

    pub fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    pub fn rate_limits(&self) -> Arc<RwLock<Option<RateLimitInfo>>> {
        Arc::clone(&self.rate_limits)
    }

    pub async fn chat(
        &mut self,
        input: ChatInput,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ChatOutcome> {
        let first_lane = input.initial_lane;
        let allow_escalation = [ComputeLane::Investigate, ComputeLane::Critical]
            .into_iter()
            .any(|lane| compute.allows_escalation(first_lane, lane));
        let first_input = self.user_input_items(&input, first_lane, compute)?;
        let thread_id = self.interactive_thread_id.clone();
        let rollover = rollover::decide(
            self.thread_context_pressure.get(&thread_id),
            self.thread_compactions
                .get(&thread_id)
                .copied()
                .unwrap_or_default(),
            self.continuity.conversation_scope(),
        );
        let needs_bridge = self.interactive_cursor.needs_bridge();
        let cursor = self.interactive_cursor.revision();
        let working_context = self
            .continuity
            .working_context(
                cursor,
                Some(&input.current_revision_id),
                input.reply_to_revision_id.as_deref(),
            )
            .await?;
        let mut outcome = self
            .run_request(
                thread_id.clone(),
                first_input,
                first_lane,
                "interactive",
                compute,
                profile,
                continuity_context,
                Some(working_context),
                rollover.as_ref(),
                allow_escalation,
                Some(input.input_events.clone()),
                &events,
            )
            .await?;
        if needs_bridge {
            self.interactive_cursor.bridge_completed();
        }
        if let Some(rollover) = rollover {
            let workspace = self.workspace.clone();
            match self.start_thread(&workspace, ToolSurface::Full).await {
                Ok(next_thread_id) => {
                    self.interactive_thread_id = next_thread_id.clone();
                    self.interactive_cursor.rotate();
                    self.clear_thread_state(&thread_id);
                    if let Some(invocation) = outcome.invocations.last_mut() {
                        push_trace_event(
                            &mut invocation.trace_events,
                            TraceEventKind::ThreadRollover,
                            "Symbiont rotated the native Codex thread",
                            json!({
                                "reason": rollover.reason(),
                                "previousThreadId": thread_id,
                                "nextThreadId": next_thread_id
                            }),
                            now(),
                        );
                    }
                }
                Err(error) => {
                    warn!(%error, "could not rotate the interactive Codex thread");
                }
            }
        }
        Ok(outcome)
    }

    pub async fn continue_conversation(
        &mut self,
        reason: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ChatOutcome> {
        let prompt = format!(
            "Reconsider one explicitly reserved short continuation after a brief pause.\n\
             Reserved reason: {reason}\n\n\
             The immediately preceding assistant message is present in the native thread or \
             supplied exact working-context bridge.\n\n\
             Make exactly one additional conversational move only if it adds a distinct \
             correction, association, or question now. Do not restate or split the prior answer, \
             browse, call tools, mention this reservation, or write a report-style preamble. If \
             nothing distinct remains, return exactly {CONTINUATION_SILENT_MARKER}."
        );
        let thread_id = self.interactive_thread_id.clone();
        let needs_bridge = self.interactive_cursor.needs_bridge();
        let working_context = if needs_bridge {
            Some(self.continuity.working_context(None, None, None).await?)
        } else {
            None
        };
        let mut outcome = self
            .run_request(
                thread_id,
                text_input_items(&prompt),
                ComputeLane::Conversation,
                "continuation",
                compute,
                profile,
                continuity_context,
                working_context,
                None,
                false,
                Some(input_events),
                &events,
            )
            .await?;
        if needs_bridge && !outcome.interrupted {
            self.interactive_cursor.bridge_completed();
        }
        if outcome.text.trim() == CONTINUATION_SILENT_MARKER {
            outcome.text.clear();
            for invocation in &mut outcome.invocations {
                invocation.produced_message = false;
            }
        }
        Ok(outcome)
    }

    pub fn mark_interactive_revision(&mut self, revision_id: String) {
        self.interactive_cursor.mark(revision_id);
    }

    pub async fn reset_interactive_thread(&mut self) -> Result<()> {
        let previous = self.interactive_thread_id.clone();
        let workspace = self.workspace.clone();
        let next = self
            .start_thread(&workspace, ToolSurface::Full)
            .await
            .context("start a fresh interactive Codex thread after message retraction")?;
        self.interactive_thread_id = next;
        self.interactive_cursor.rotate();
        self.clear_thread_state(&previous);
        Ok(())
    }

    pub async fn explore(
        &mut self,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ExplorationOutcome> {
        let prompt = scout_prompt(AUTONOMOUS_SILENT_MARKER, AUTONOMOUS_SUPERSEDED_MARKER);
        let scout_thread_id = self.autonomous_scout_thread_id.clone();
        let scout = self
            .run_request(
                scout_thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Observe,
                "autonomous_scout",
                compute,
                profile,
                continuity_context,
                None,
                None,
                false,
                Some(input_events.clone()),
                &events,
            )
            .await?;
        if !scout.interrupted {
            self.renew_background_thread(&scout_thread_id, BackgroundThread::AutonomousScout)
                .await;
        }
        let superseded = is_superseded_autonomous_response(&scout.text);
        let finding = (!scout.interrupted && !superseded)
            .then(|| finding_from_invocations(&scout.invocations))
            .transpose()?
            .flatten();
        let root_id = scout.invocations.first().map(|run| run.id.clone());
        let mut invocations = scout.invocations;
        for invocation in &mut invocations {
            invocation.produced_message = false;
        }

        let mut interrupted = scout.interrupted;
        let mut outreach = None;
        let mut hunch_revisions = Vec::new();
        if let Some(finding) = finding {
            let review_lane = self
                .compute_policies
                .match_texts(finding.routing_texts())
                .await
                .map(|matched| matched.policy.minimum_lane)
                .unwrap_or(ComputeLane::Conversation);
            let prompt = review_prompt(&finding, AUTONOMOUS_SILENT_MARKER)?;
            let review_thread_id = self.autonomous_review_thread_id.clone();
            let mut review = self
                .run_request(
                    review_thread_id.clone(),
                    text_input_items(&prompt),
                    review_lane,
                    "autonomous",
                    compute,
                    profile,
                    continuity_context,
                    None,
                    None,
                    true,
                    Some(input_events),
                    &events,
                )
                .await?;
            if !review.interrupted {
                self.renew_background_thread(&review_thread_id, BackgroundThread::AutonomousReview)
                    .await;
            }
            interrupted = review.interrupted;
            let candidate = (!review.interrupted)
                .then(|| proactive_message_candidate(&review.invocations))
                .flatten();
            if let Some(candidate) = candidate {
                outreach = Some(candidate);
            }
            hunch_revisions = successful_hunch_revisions(&review.invocations);
            for invocation in &mut review.invocations {
                invocation.parent_id.clone_from(&root_id);
                invocation.produced_message = false;
            }
            invocations.extend(review.invocations);
        }

        let metadata = metadata_for(&invocations, "autonomous");
        let context_revision_ids = context_revision_ids(&invocations);
        Ok(ExplorationOutcome {
            outreach,
            metadata,
            invocations,
            context_revision_ids,
            hunch_revisions,
            superseded,
            interrupted,
        })
    }

    pub async fn sense(
        &mut self,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        sensing_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<SensingOutcome> {
        let thread_id = self.ambient_sense_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&sensing_prompt(AUTONOMOUS_SILENT_MARKER)),
                ComputeLane::Sense,
                "ambient_sense",
                compute,
                profile,
                sensing_context,
                None,
                None,
                false,
                Some(input_events),
                &events,
            )
            .await?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::AmbientSense)
                .await;
        }
        let candidates = if outcome.interrupted {
            Vec::new()
        } else {
            sensing_candidates_from_invocations(&outcome.invocations)?
        };
        let mut invocations = outcome.invocations;
        for invocation in &mut invocations {
            invocation.produced_message = false;
        }
        Ok(SensingOutcome {
            candidates,
            invocations,
            interrupted: outcome.interrupted,
        })
    }

    pub async fn maintain_summary(
        &mut self,
        target_revision_id: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<MaintenanceOutcome> {
        let prompt = summary_maintenance_prompt(target_revision_id, MAINTENANCE_COMPLETE_MARKER);
        let thread_id = self.maintenance_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Observe,
                "maintenance",
                compute,
                profile,
                continuity_context,
                None,
                None,
                false,
                Some(input_events),
                &events,
            )
            .await;
        let mut outcome = outcome?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
                .await;
        }
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
        let summarized = outcome.invocations.iter().any(|invocation| {
            invocation.trace_steps.iter().any(|step| {
                step.namespace == "pcp"
                    && step.tool == "write_summary"
                    && step.succeeded
                    && step.arguments.get("target_page_id").and_then(Value::as_str)
                        == Some(target_revision_id)
            })
        });
        let model = outcome
            .invocations
            .last()
            .map(|invocation| invocation.effective_model.clone());
        Ok(MaintenanceOutcome {
            invocations: outcome.invocations,
            summarized,
            model,
            interrupted: outcome.interrupted,
        })
    }

    pub async fn maintain_symbiont_context(
        &mut self,
        source_bundle: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ContextMaintenanceOutcome> {
        let prompt = context_maintenance_prompt(source_bundle, CONTEXT_MAINTENANCE_COMPLETE_MARKER);
        let thread_id = self.maintenance_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Observe,
                "maintenance",
                compute,
                profile,
                continuity_context,
                None,
                None,
                false,
                Some(input_events),
                &events,
            )
            .await;
        let mut outcome = outcome?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
                .await;
        }
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
        Ok(ContextMaintenanceOutcome {
            current_map_updated: successful_symbiont_tool(
                &outcome.invocations,
                "update_current_map",
            ),
            open_loops_updated: successful_symbiont_tool(&outcome.invocations, "update_open_loops"),
            invocations: outcome.invocations,
            interrupted: outcome.interrupted,
        })
    }

    pub async fn review_profile(
        &mut self,
        source_bundle: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ProfileReviewOutcome> {
        let prompt = profile_review_prompt(source_bundle, PROFILE_REVIEW_COMPLETE_MARKER);
        let thread_id = self.maintenance_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Critical,
                "maintenance",
                compute,
                profile,
                continuity_context,
                None,
                None,
                false,
                Some(input_events),
                &events,
            )
            .await;
        let mut outcome = outcome?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
                .await;
        }
        let review_step = outcome
            .invocations
            .iter()
            .flat_map(|invocation| invocation.trace_steps.iter())
            .find(|step| {
                step.namespace == "symbiont"
                    && step.tool == "record_profile_review"
                    && step.succeeded
            });
        let status = review_step
            .and_then(|step| step.arguments.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let clarification_question = if status.as_deref() == Some("clarification") {
            review_step
                .and_then(|step| step.arguments.get("clarification_question"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|question| !question.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
        if clarification_question.is_some()
            && let Some(invocation) = outcome.invocations.last_mut()
        {
            invocation.produced_message = true;
        }
        let metadata = metadata_for(&outcome.invocations, "maintenance");
        let context_revision_ids = context_revision_ids(&outcome.invocations);
        Ok(ProfileReviewOutcome {
            invocations: outcome.invocations,
            status,
            clarification_question,
            metadata,
            context_revision_ids,
            interrupted: outcome.interrupted,
        })
    }

    pub async fn reflect_interaction(
        &mut self,
        source_bundle: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ReflectionOutcome> {
        let prompt = interaction_reflection_prompt(source_bundle, REFLECTION_COMPLETE_MARKER);
        let thread_id = self.maintenance_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Observe,
                "reflection",
                compute,
                profile,
                continuity_context,
                None,
                None,
                false,
                Some(input_events),
                &events,
            )
            .await;
        let mut outcome = outcome?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
                .await;
        }
        let reflection_steps = outcome
            .invocations
            .iter()
            .flat_map(|invocation| invocation.trace_steps.iter())
            .filter(|step| {
                (step.namespace == "symbiont"
                    && matches!(
                        step.tool.as_str(),
                        "upsert_episode"
                            | "upsert_interaction_hypothesis"
                            | "schedule_follow_up"
                            | "request_exploration"
                            | PROPOSE_OUTREACH_TOOL
                            | "complete_reflection"
                    ))
                    || (step.namespace == "pcp" && step.tool == "assess_validity")
            })
            .filter(|step| step.succeeded)
            .collect::<Vec<_>>();
        let summary = reflection_steps
            .iter()
            .rev()
            .find(|step| step.tool == "complete_reflection")
            .and_then(|step| step.arguments.get("summary"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .map(str::to_owned);
        let actions = reflection_steps
            .iter()
            .filter(|step| step.tool != "complete_reflection")
            .map(|step| format!("{}.{}", step.namespace, step.tool))
            .collect();
        let outreach = reflection_steps
            .iter()
            .rev()
            .find(|step| step.tool == PROPOSE_OUTREACH_TOOL)
            .and_then(|step| OutreachCandidate::from_tool_arguments(&step.arguments));
        let metadata = metadata_for(&outcome.invocations, "reflection");
        let context_revision_ids = context_revision_ids(&outcome.invocations);
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
        Ok(ReflectionOutcome {
            invocations: outcome.invocations,
            summary,
            actions,
            metadata,
            outreach,
            context_revision_ids,
            interrupted: outcome.interrupted,
        })
    }

    pub async fn reconcile_memory(
        &mut self,
        request: ReconciliationModelRequest<'_>,
    ) -> Result<ReconciliationModelOutcome> {
        let prompt = memory_reconciliation_prompt(
            request.mode,
            request.run_id,
            request.inventory_bundle,
            request.proposals,
            RECONCILIATION_COMPLETE_MARKER,
        );
        let origin = match request.mode {
            ReconciliationMode::Preview => "reconciliation_preview",
            ReconciliationMode::Apply => "reconciliation_apply",
        };
        let lane = match request.mode {
            ReconciliationMode::Preview => ComputeLane::Investigate,
            ReconciliationMode::Apply => ComputeLane::Critical,
        };
        let thread_id = self.maintenance_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                lane,
                origin,
                request.compute,
                request.profile,
                request.continuity_context,
                None,
                None,
                false,
                Some(request.input_events),
                &request.events,
            )
            .await;
        let mut outcome = outcome?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
                .await;
        }
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
        let completion = outcome
            .invocations
            .iter()
            .flat_map(|invocation| &invocation.trace_steps)
            .rev()
            .find(|step| {
                step.succeeded
                    && step.namespace == "symbiont"
                    && step.tool == "complete_reconciliation"
            });
        let summary = completion
            .and_then(|step| step.arguments.get("summary"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let proposals = completion
            .and_then(|step| step.arguments.get("proposals"))
            .cloned()
            .map(serde_json::from_value::<Vec<ReconciliationProposal>>)
            .transpose()
            .context("parse memory reconciliation proposals")?
            .unwrap_or_default();
        let actions = reconciliation_actions(&outcome.invocations);
        Ok(ReconciliationModelOutcome {
            invocations: outcome.invocations,
            summary,
            proposals,
            actions,
            interrupted: outcome.interrupted,
        })
    }

    pub async fn evaluate_pcp_maintenance(
        &mut self,
        request: PcpMaintenanceModelRequest<'_>,
    ) -> Result<PcpMaintenanceModelOutcome> {
        let prompt =
            pcp_maintenance_worker_prompt(request.request, PCP_MAINTENANCE_COMPLETE_MARKER)?;
        let thread_id = self.pcp_maintenance_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Investigate,
                "pcp_maintenance",
                request.compute,
                request.profile,
                "",
                None,
                None,
                false,
                Some(request.input_events),
                &request.events,
            )
            .await?;
        if !outcome.interrupted {
            self.renew_background_thread(&thread_id, BackgroundThread::PcpMaintenance)
                .await;
        }
        let completion = outcome
            .invocations
            .iter()
            .flat_map(|invocation| &invocation.trace_steps)
            .rev()
            .find(|step| {
                step.succeeded
                    && step.namespace == "symbiont"
                    && step.tool == "complete_pcp_maintenance"
            });
        let response = if outcome.interrupted {
            pcp_runtime::MaintenanceWorkerResponse::Defer {
                reason: Some("superseded by newer user input".to_owned()),
            }
        } else {
            completion
                .map(|step| step.arguments.clone())
                .map(serde_json::from_value::<pcp_runtime::MaintenanceWorkerResponse>)
                .transpose()
                .context("parse PCP semantic maintenance decision")?
                .context("PCP semantic maintenance completed without a decision")?
        };
        let mut invocations = outcome.invocations;
        for invocation in &mut invocations {
            invocation.produced_message = false;
        }
        Ok(PcpMaintenanceModelOutcome {
            response,
            invocations,
            interrupted: outcome.interrupted,
        })
    }

    async fn renew_background_thread(&mut self, previous: &str, slot: BackgroundThread) {
        let workspace = self.workspace.clone();
        let tool_surface = match slot {
            BackgroundThread::AmbientSense => ToolSurface::AmbientSense,
            BackgroundThread::AutonomousScout => ToolSurface::AutonomousScout,
            BackgroundThread::AutonomousReview | BackgroundThread::Maintenance => ToolSurface::Full,
            BackgroundThread::PcpMaintenance => ToolSurface::PcpMaintenance,
        };
        match self.start_thread(&workspace, tool_surface).await {
            Ok(next) => {
                match slot {
                    BackgroundThread::AmbientSense => self.ambient_sense_thread_id = next,
                    BackgroundThread::AutonomousScout => self.autonomous_scout_thread_id = next,
                    BackgroundThread::AutonomousReview => self.autonomous_review_thread_id = next,
                    BackgroundThread::Maintenance => self.maintenance_thread_id = next,
                    BackgroundThread::PcpMaintenance => self.pcp_maintenance_thread_id = next,
                }
                self.clear_thread_state(previous);
            }
            Err(error) => {
                warn!(%error, "could not renew a stateless background Codex thread");
            }
        }
    }

    async fn run_request(
        &mut self,
        thread_id: String,
        first_input: Vec<Value>,
        first_lane: ComputeLane,
        origin: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        working_context: Option<WorkingContext>,
        rollover: Option<&RolloverDecision>,
        allow_escalation: bool,
        mut input_events: Option<watch::Receiver<u64>>,
        events: &mpsc::Sender<RuntimeEvent>,
    ) -> Result<ChatOutcome> {
        let mut first = self
            .run_turn(
                &thread_id,
                first_input,
                first_lane,
                origin,
                compute,
                profile,
                continuity_context,
                working_context.as_ref(),
                rollover,
                allow_escalation,
                None,
                input_events.as_mut(),
                events,
            )
            .await?;
        let root_id = first.invocation.id.clone();
        let mut invocations = Vec::new();
        if first.interrupted {
            first.invocation.produced_message = false;
            invocations.push(first.invocation);
            return Ok(interrupted_chat_outcome(invocations, origin));
        }

        let (final_text, generated_images) = if let Some(escalation) = first.escalation.take() {
            if compute.allows_escalation(first_lane, escalation.lane) {
                let deep_rollover =
                    rollover.filter(|_| !invocation_wrote_checkpoint(&first.invocation));
                first.invocation.produced_message = false;
                invocations.push(first.invocation);
                send_event(events, RuntimeEvent::Reset).await;
                let follow_up = format!(
                    "Continue the preceding {} request using the approved {} compute lane. \
                     The escalation reason was: {}. Return only the final result. \
                     Do not mention routing, models, internal lanes, or this continuation message.",
                    origin,
                    escalation.lane.as_str(),
                    escalation.reason
                );
                let mut deep = self
                    .run_turn(
                        &thread_id,
                        text_input_items(&follow_up),
                        escalation.lane,
                        origin,
                        compute,
                        profile,
                        continuity_context,
                        None,
                        deep_rollover,
                        false,
                        None,
                        input_events.as_mut(),
                        events,
                    )
                    .await?;
                if deep.interrupted {
                    deep.invocation.parent_id = Some(root_id);
                    deep.invocation.produced_message = false;
                    invocations.push(deep.invocation);
                    return Ok(interrupted_chat_outcome(invocations, origin));
                }
                deep.invocation.parent_id = Some(root_id);
                deep.invocation.produced_message = true;
                let text = deep.text;
                let generated_images = deep.generated_images;
                invocations.push(deep.invocation);
                (text, generated_images)
            } else {
                first.invocation.produced_message = true;
                let text = first.text;
                let generated_images = first.generated_images;
                invocations.push(first.invocation);
                (text, generated_images)
            }
        } else {
            first.invocation.produced_message = true;
            let text = first.text;
            let generated_images = first.generated_images;
            invocations.push(first.invocation);
            (text, generated_images)
        };

        let (disposition, final_text) = if origin == "interactive" && generated_images.is_empty() {
            interpret_interactive_output(final_text)
        } else {
            (ChatDisposition::Reply, final_text)
        };
        if let Some(invocation) = invocations.last_mut() {
            invocation.produced_message = disposition.produces_message();
            if !disposition.produces_message()
                && let Some(event) = invocation
                    .trace_events
                    .iter_mut()
                    .rev()
                    .find(|event| matches!(&event.kind, TraceEventKind::AgentMessage))
            {
                event.kind = TraceEventKind::TurnSettled;
                event.title = if disposition.reaction().is_some() {
                    "Emoji reaction completed the turn".to_owned()
                } else {
                    "Turn settled without an assistant message".to_owned()
                };
                event.details = json!({"reaction": disposition.reaction()});
            }
        }
        let metadata = metadata_for(&invocations, origin);
        let context_revision_ids = context_revision_ids(&invocations);
        let scheduled_follow_up_ids =
            successful_tool_result_ids(&invocations, "symbiont", "schedule_follow_up", "id");
        let reserved_continuation_ids =
            successful_tool_result_ids(&invocations, "symbiont", "reserve_continuation", "id");
        let requested_exploration_ids =
            successful_tool_result_ids(&invocations, "symbiont", "request_exploration", "id");
        Ok(ChatOutcome {
            text: final_text,
            disposition,
            generated_images,
            metadata,
            invocations,
            context_revision_ids,
            scheduled_follow_up_ids,
            reserved_continuation_ids,
            requested_exploration_ids,
            interrupted: false,
        })
    }

    async fn run_turn(
        &mut self,
        thread_id: &str,
        input: Vec<Value>,
        lane: ComputeLane,
        origin: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        working_context: Option<&WorkingContext>,
        rollover: Option<&RolloverDecision>,
        allow_escalation: bool,
        overrides: Option<&TurnOverrides>,
        mut input_events: Option<&mut watch::Receiver<u64>>,
        events: &mpsc::Sender<RuntimeEvent>,
    ) -> Result<TurnOutcome> {
        let lane_config = compute.lane(lane).clone();
        let model = self.model_info(&lane_config.model)?;
        send_event(
            events,
            RuntimeEvent::Activity {
                label: format!("{} 正在思考", model.display_name),
                model: model.model.clone(),
                display_name: model.display_name.clone(),
                effort: lane_config.effort.clone(),
                lane: lane.as_str().to_owned(),
            },
        )
        .await;

        let started = Instant::now();
        let started_at = now();
        let baseline_usage = self
            .thread_usage
            .get(thread_id)
            .cloned()
            .unwrap_or_default();
        let mut turn_usage = TokenBreakdown::default();
        let (observable_history_tail, history_tail_truncated) =
            match self.observable_history_tail(thread_id).await {
                Ok(history) => history,
                Err(error) => {
                    debug!(%error, "Codex did not expose its observable thread history");
                    (Vec::new(), false)
                }
            };
        let mut fragments = if origin == "pcp_maintenance" {
            Vec::new()
        } else {
            context_fragments(
                lane,
                allow_escalation,
                profile,
                continuity_context,
                working_context,
                rollover,
            )
        };
        if origin == "interactive" {
            fragments.push(ContextFragment {
                source: "symbiont.interaction".to_owned(),
                kind: "application".to_owned(),
                value: interaction_disposition_prompt(),
            });
        }
        let mut context_snapshot = ContextSnapshot {
            input: input.clone(),
            fragments: fragments.clone(),
            working_context: working_context.cloned(),
            developer_instructions: if origin == "pcp_maintenance" {
                pcp_maintenance_developer_instructions().to_owned()
            } else {
                developer_instructions()
            },
            native_thread: NativeThreadSnapshot {
                thread_id: thread_id.to_owned(),
                cursor_before: working_context.and_then(|context| context.cursor_before.clone()),
                prior_turns: self
                    .thread_turns
                    .get(thread_id)
                    .copied()
                    .unwrap_or_default(),
                compactions_before: self
                    .thread_compactions
                    .get(thread_id)
                    .copied()
                    .unwrap_or_default(),
                model_context_window: None,
                exact_prompt_available: false,
                observable_history_tail,
                history_tail_truncated,
            },
        };
        let mut params = Map::new();
        params.insert("threadId".to_owned(), json!(thread_id));
        params.insert("input".to_owned(), Value::Array(input));
        params.insert("model".to_owned(), json!(lane_config.model));
        params.insert("effort".to_owned(), json!(lane_config.effort));
        params.insert("summary".to_owned(), json!("concise"));
        params.insert(
            "additionalContext".to_owned(),
            additional_context_value(&fragments),
        );
        params.insert(
            "responsesapiClientMetadata".to_owned(),
            json!({
                "symbiont_origin": origin,
                "symbiont_lane": lane.as_str()
            }),
        );
        if let Some(service_tier) = lane_config.service_tier.as_deref() {
            params.insert("serviceTier".to_owned(), json!(service_tier));
        }
        if let Some(overrides) = overrides {
            params.insert("cwd".to_owned(), json!(overrides.cwd));
            params.insert("approvalPolicy".to_owned(), granular_approval_policy());
            params.insert("sandboxPolicy".to_owned(), overrides.sandbox_policy.clone());
        }

        let request_id = self
            .send_request("turn/start", Value::Object(params))
            .await?;
        let mut turn_id: Option<String> = None;
        let mut response_text = String::new();
        let mut generated_images = Vec::new();
        let mut escalation = None;
        let mut trace_steps = Vec::new();
        let mut trace_events = Vec::new();
        let mut tool_deduplicator = TurnToolDeduplicator::default();
        let mut effective_model = lane_config.model.clone();
        let mut interrupt_requested = false;
        let mut interrupt_sent = false;
        let mut interactive_delta_gate = InteractiveDeltaGate::new(origin == "interactive");

        loop {
            let message = if interrupt_sent || input_events.is_none() {
                self.read_message().await?
            } else {
                tokio::select! {
                    message = self.read_message() => message?,
                    changed = input_events.as_mut().expect("input receiver checked").changed() => {
                        if changed.is_err() {
                            input_events = None;
                            continue;
                        }
                        interrupt_requested = true;
                        if let Some(turn_id) = turn_id.as_deref() {
                            self.send_request(
                                "turn/interrupt",
                                json!({"threadId": thread_id, "turnId": turn_id}),
                            )
                            .await?;
                            interrupt_sent = true;
                        }
                        continue;
                    }
                }
            };
            if self
                .handle_server_request(
                    &message,
                    lane,
                    compute,
                    &effective_model,
                    origin,
                    allow_escalation,
                    &mut escalation,
                    &mut tool_deduplicator,
                    &mut trace_steps,
                    &mut trace_events,
                )
                .await?
            {
                continue;
            }

            if message.get("id") == Some(&json!(request_id)) {
                if let Some(error) = message.get("error") {
                    anyhow::bail!("Codex rejected turn/start: {}", error_message(error));
                }
                turn_id = message
                    .pointer("/result/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if interrupt_requested
                    && !interrupt_sent
                    && let Some(turn_id) = turn_id.as_deref()
                {
                    self.send_request(
                        "turn/interrupt",
                        json!({"threadId": thread_id, "turnId": turn_id}),
                    )
                    .await?;
                    interrupt_sent = true;
                }
                continue;
            }

            let method = message.get("method").and_then(Value::as_str);
            let params = message.get("params").unwrap_or(&Value::Null);
            match method {
                Some("item/started") if event_matches(params, thread_id, &turn_id) => {
                    if let Some(label) = activity_label(params.pointer("/item")) {
                        send_event(
                            events,
                            RuntimeEvent::Activity {
                                label,
                                model: effective_model.clone(),
                                display_name: self.model_display_name(&effective_model),
                                effort: lane_config.effort.clone(),
                                lane: lane.as_str().to_owned(),
                            },
                        )
                        .await;
                    }
                }
                Some("item/agentMessage/delta") if event_matches(params, thread_id, &turn_id) => {
                    if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                        response_text.push_str(delta);
                        if let Some(text) = interactive_delta_gate.push(delta) {
                            send_event(events, RuntimeEvent::Delta { text }).await;
                        }
                    }
                }
                Some("thread/tokenUsage/updated") if event_matches(params, thread_id, &turn_id) => {
                    if let Some(last) = params.pointer("/tokenUsage/last") {
                        turn_usage = TokenBreakdown::from_value(last);
                    }
                    context_snapshot.native_thread.model_context_window = params
                        .pointer("/tokenUsage/modelContextWindow")
                        .and_then(Value::as_u64);
                    if let Some(total) = params.pointer("/tokenUsage/total") {
                        self.thread_usage
                            .insert(thread_id.to_owned(), TokenBreakdown::from_value(total));
                    }
                }
                Some("item/completed") if event_matches(params, thread_id, &turn_id) => {
                    if let Some(item) = params.get("item") {
                        if let Some(image) = generated_image_output(item) {
                            remember_generated_image(&mut generated_images, image);
                        }
                        if item.get("type").and_then(Value::as_str) == Some("contextCompaction") {
                            *self
                                .thread_compactions
                                .entry(thread_id.to_owned())
                                .or_default() += 1;
                        }
                        if let Some((kind, title, details)) = observable_item_event(item) {
                            push_trace_event(
                                &mut trace_events,
                                kind,
                                title,
                                details,
                                timestamp_from_millis(
                                    params.get("completedAtMs").and_then(Value::as_i64),
                                ),
                            );
                        }
                    }
                }
                Some("thread/compacted") if event_matches(params, thread_id, &turn_id) => {
                    *self
                        .thread_compactions
                        .entry(thread_id.to_owned())
                        .or_default() += 1;
                    push_trace_event(
                        &mut trace_events,
                        TraceEventKind::ContextCompaction,
                        "Codex compacted the native thread context",
                        json!({}),
                        now(),
                    );
                }
                Some("account/rateLimits/updated") => {
                    if let Some(rate_limits) = params.get("rateLimits") {
                        self.set_rate_limits(rate_limits).await;
                    }
                }
                Some("model/rerouted") if event_matches(params, thread_id, &turn_id) => {
                    if let Some(to_model) = params.get("toModel").and_then(Value::as_str) {
                        push_trace_event(
                            &mut trace_events,
                            TraceEventKind::ModelReroute,
                            "Codex rerouted the model",
                            params.clone(),
                            now(),
                        );
                        effective_model = to_model.to_owned();
                    }
                }
                Some("turn/completed") if completed_turn_matches(params, thread_id, &turn_id) => {
                    if let Some(items) = params.pointer("/turn/items").and_then(Value::as_array) {
                        for item in items {
                            if let Some(image) = generated_image_output(item) {
                                remember_generated_image(&mut generated_images, image);
                            }
                        }
                    }
                    let status = params
                        .pointer("/turn/status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    if status != "completed" && status != "interrupted" {
                        let error = params
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("Codex turn did not complete");
                        anyhow::bail!("{error}");
                    }
                    let interrupted = status == "interrupted";
                    let response_text = completed_response_text(params, &response_text);
                    if let Some(text) = interactive_delta_gate.finish(&response_text) {
                        send_event(events, RuntimeEvent::Delta { text }).await;
                    }
                    if response_text.is_empty() && !interrupted {
                        anyhow::bail!("Codex completed without an assistant message");
                    }
                    if interrupted {
                        push_trace_event(
                            &mut trace_events,
                            TraceEventKind::TurnInterrupted,
                            "Codex turn interrupted by newer user input",
                            json!({}),
                            now(),
                        );
                    } else {
                        push_trace_event(
                            &mut trace_events,
                            TraceEventKind::AgentMessage,
                            "Final assistant message",
                            json!({"text": response_text.clone()}),
                            now(),
                        );
                    }
                    let turn_id = turn_id
                        .or_else(|| {
                            params
                                .pointer("/turn/id")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .context("completed Codex turn omitted its id")?;
                    let usage = if turn_usage.total_tokens > 0 {
                        turn_usage
                    } else {
                        self.thread_usage
                            .get(thread_id)
                            .cloned()
                            .unwrap_or_default()
                            .difference(&baseline_usage)
                    };
                    if let Some(context_window) =
                        context_snapshot.native_thread.model_context_window
                    {
                        self.thread_context_pressure.insert(
                            thread_id.to_owned(),
                            ThreadContextPressure {
                                input_tokens: usage.input_tokens,
                                context_window,
                            },
                        );
                    }
                    *self.thread_turns.entry(thread_id.to_owned()).or_default() += 1;
                    let model_display_name = self.model_display_name(&effective_model);
                    let mut invocation = invocation_record(
                        thread_id,
                        &turn_id,
                        lane,
                        origin,
                        &lane_config,
                        &effective_model,
                        &model_display_name,
                        &started_at,
                        started.elapsed(),
                        usage,
                        trace_steps,
                        Some(context_snapshot),
                        trace_events,
                    );
                    if interrupted {
                        invocation.status = "interrupted".to_owned();
                    }
                    return Ok(TurnOutcome {
                        text: response_text,
                        generated_images,
                        invocation,
                        escalation,
                        interrupted,
                    });
                }
                Some("error") if event_matches(params, thread_id, &turn_id) => {
                    let message = params
                        .pointer("/error/message")
                        .or_else(|| params.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown Codex error");
                    anyhow::bail!("{message}");
                }
                _ => {}
            }
        }
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "symbiont_d",
                    "title": "symbiont-d",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true
                }
            }),
        )
        .await
        .context("initialize Codex app-server")?;
        self.send_notification("initialized", json!({})).await
    }

    async fn load_models(&mut self) -> Result<Vec<ModelInfo>> {
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let result = self
                .request(
                    "model/list",
                    json!({
                        "cursor": cursor,
                        "includeHidden": false,
                        "limit": 100
                    }),
                )
                .await
                .context("list Codex models")?;
            for value in result
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                models.push(ModelInfo::from_app_server(value)?);
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        if models.is_empty() {
            anyhow::bail!("Codex model/list returned no visible models");
        }
        Ok(models)
    }

    async fn refresh_rate_limits(&mut self) {
        match self.request("account/rateLimits/read", json!({})).await {
            Ok(result) => {
                if let Some(rate_limits) = result.get("rateLimits") {
                    self.set_rate_limits(rate_limits).await;
                }
            }
            Err(error) => warn!(%error, "could not read Codex rate limits"),
        }
    }

    async fn set_rate_limits(&self, value: &Value) {
        *self.rate_limits.write().await = Some(RateLimitInfo {
            limit_id: optional_text(value, "limitId"),
            plan_type: optional_text(value, "planType"),
            used_percent: value
                .pointer("/primary/usedPercent")
                .and_then(Value::as_f64),
            window_minutes: value
                .pointer("/primary/windowDurationMins")
                .and_then(Value::as_u64),
            resets_at: value.pointer("/primary/resetsAt").and_then(Value::as_i64),
        });
    }

    async fn start_thread(
        &mut self,
        workspace: &PathBuf,
        tool_surface: ToolSurface,
    ) -> Result<String> {
        let instructions = match tool_surface {
            ToolSurface::AmbientSense => ambient_sense_developer_instructions().to_owned(),
            ToolSurface::PcpMaintenance => pcp_maintenance_developer_instructions().to_owned(),
            ToolSurface::Full | ToolSurface::AutonomousScout => developer_instructions(),
        };
        let dynamic_tools = match tool_surface {
            ToolSurface::Full => SymbiontTools::specifications(),
            ToolSurface::AmbientSense => SymbiontTools::sensing_specifications(),
            ToolSurface::AutonomousScout => SymbiontTools::scout_specifications(),
            ToolSurface::PcpMaintenance => SymbiontTools::pcp_maintenance_specifications(),
        };
        let result = self
            .request(
                "thread/start",
                json!({
                    "cwd": workspace,
                    "approvalPolicy": granular_approval_policy(),
                    "sandbox": "read-only",
                    "ephemeral": true,
                    "serviceName": "symbiont-d",
                    "developerInstructions": instructions,
                    "config": {
                        "web_search": "live"
                    },
                    "dynamicTools": dynamic_tools
                }),
            )
            .await
            .context("start the symbiont Codex thread")?;
        result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("thread/start response omitted thread.id")
    }

    fn clear_thread_state(&mut self, thread_id: &str) {
        self.thread_usage.remove(thread_id);
        self.thread_turns.remove(thread_id);
        self.thread_compactions.remove(thread_id);
        self.thread_context_pressure.remove(thread_id);
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let request_id = self.send_request(method, params).await?;
        loop {
            let message = self.read_message().await?;
            if self.handle_unexpected_server_request(&message).await? {
                tokio::task::yield_now().await;
                continue;
            }
            if message.get("id") != Some(&json!(request_id)) {
                tokio::task::yield_now().await;
                continue;
            }
            if let Some(error) = message.get("error") {
                anyhow::bail!("{method} failed: {}", error_message(error));
            }
            return message
                .get("result")
                .cloned()
                .with_context(|| format!("{method} response omitted result"));
        }
    }

    async fn observable_history_tail(&mut self, thread_id: &str) -> Result<(Vec<Value>, bool)> {
        let result = self
            .request(
                "thread/items/list",
                json!({
                    "threadId": thread_id,
                    "limit": 24,
                    "sortDirection": "desc"
                }),
            )
            .await
            .context("read observable Codex thread history")?;
        let truncated = result
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null());
        let mut items = result
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(observable_history_item)
            .collect::<Vec<_>>();
        items.reverse();
        Ok((items, truncated))
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
        let mut encoded = serde_json::to_vec(message).context("encode app-server message")?;
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .await
            .context("write to Codex app-server")?;
        self.stdin.flush().await.context("flush Codex request")
    }

    async fn read_message(&mut self) -> Result<Value> {
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .context("read from Codex app-server")?
                .context("Codex app-server closed its output")?;
            if line.trim().is_empty() {
                continue;
            }
            debug!(message = %line, "received app-server message");
            match serde_json::from_str(&line) {
                Ok(message) => return Ok(message),
                Err(error) => warn!(%error, "ignored non-JSON app-server output"),
            }
        }
    }

    async fn handle_server_request(
        &mut self,
        message: &Value,
        current_lane: ComputeLane,
        compute: &ComputeConfig,
        effective_model: &str,
        run_origin: &str,
        allow_escalation: bool,
        escalation: &mut Option<EscalationRequest>,
        tool_deduplicator: &mut TurnToolDeduplicator,
        trace_steps: &mut Vec<ToolTraceStep>,
        trace_events: &mut Vec<ExecutionTraceEvent>,
    ) -> Result<bool> {
        if let Some(request) = permission_request(message, run_origin) {
            let id = message
                .get("id")
                .cloned()
                .context("permission request omitted id")?;
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            push_trace_event(
                trace_events,
                TraceEventKind::PermissionRequest,
                "Codex requested host permission",
                json!({
                    "method": method,
                    "params": params
                }),
                now(),
            );
            let resolution = self.permissions.request(request).await;
            let response = approval_response(message, resolution.decision)
                .context("unsupported Codex permission request")?;
            push_trace_event(
                trace_events,
                TraceEventKind::PermissionResolution,
                "Host resolved Codex permission request",
                json!({
                    "method": method,
                    "decision": resolution.decision,
                    "source": resolution.source
                }),
                now(),
            );
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
        let params = message.get("params").unwrap_or(&Value::Null);
        let started = Instant::now();
        let started_at = now();
        let requested_namespace = params
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("symbiont");
        let requested_tool = params
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let arguments = normalize_trace_arguments(params.get("arguments"));
        let tool_sequence = trace_steps.len() as u32;
        let duplicate_from =
            match tool_deduplicator.plan(requested_namespace, requested_tool, &arguments) {
                ToolCallPlan::Execute => None,
                ToolCallPlan::Reuse { original_sequence } => Some(original_sequence),
            };
        let (tool_name, succeeded, mut response, execution_escalation) =
            if let Some(original_sequence) = duplicate_from {
                (
                    format!("{requested_namespace}.{requested_tool}"),
                    true,
                    tool_result(
                        true,
                        format!(
                            "This exact PCP call already succeeded as tool call {} in this turn. \
                             Reuse that earlier result; the host did not execute this duplicate.",
                            original_sequence + 1
                        ),
                    ),
                    None,
                )
            } else {
                let execution = self
                    .tools
                    .execute_for_model(params, Some(effective_model), run_origin)
                    .await;
                (
                    execution.tool_name,
                    execution.succeeded,
                    execution.response,
                    execution.escalation,
                )
            };
        if let Some(request) = execution_escalation {
            if allow_escalation
                && escalation.is_none()
                && compute.allows_escalation(current_lane, request.lane)
            {
                *escalation = Some(request);
            } else {
                response = tool_result(
                    false,
                    "The configured compute policy does not allow this escalation.".to_owned(),
                );
            }
        }
        let (namespace, tool) = tool_name
            .split_once('.')
            .map(|(namespace, tool)| (namespace.to_owned(), tool.to_owned()))
            .unwrap_or_else(|| ("unknown".to_owned(), tool_name.clone()));
        let response_succeeded = succeeded
            && response
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if duplicate_from.is_none() && response_succeeded {
            tool_deduplicator.remember_success(&namespace, &tool, &arguments, tool_sequence);
        }
        let mut trace_result = response.clone();
        if let Some(original_sequence) = duplicate_from
            && let Some(result) = trace_result.as_object_mut()
        {
            result.insert(
                "_symbiontTrace".to_owned(),
                json!({
                    "deduplicated": true,
                    "reusedFromSequence": original_sequence
                }),
            );
        }
        trace_steps.push(ToolTraceStep {
            sequence: tool_sequence,
            namespace,
            tool,
            started_at,
            completed_at: now(),
            duration_ms: started.elapsed().as_millis() as u64,
            succeeded: response_succeeded,
            arguments,
            result: trace_result,
        });
        let step = trace_steps
            .last()
            .expect("the tool trace step was just appended");
        push_trace_event(
            trace_events,
            TraceEventKind::ToolCall,
            format!("{}.{}", step.namespace, step.tool),
            json!({
                "toolSequence": tool_sequence,
                "succeeded": step.succeeded,
                "durationMs": step.duration_ms,
                "deduplicated": duplicate_from.is_some(),
                "reusedFromSequence": duplicate_from
            }),
            step.completed_at.clone(),
        );
        self.send_json(&json!({
            "id": id,
            "result": response
        }))
        .await?;
        Ok(true)
    }

    async fn handle_unexpected_server_request(&mut self, message: &Value) -> Result<bool> {
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
            "result": tool_result(false, "No active symbiont turn can handle this tool call.".to_owned())
        }))
        .await?;
        Ok(true)
    }

    fn model_info(&self, slug: &str) -> Result<&ModelInfo> {
        self.models
            .iter()
            .find(|model| model.model == slug || model.id == slug)
            .with_context(|| format!("configured model is no longer available: {slug}"))
    }

    fn user_input_items(
        &self,
        input: &ChatInput,
        lane: ComputeLane,
        compute: &ComputeConfig,
    ) -> Result<Vec<Value>> {
        self.input_items_with_local_images(&input.text, &input.local_images, lane, compute)
    }

    fn input_items_with_local_images(
        &self,
        text: &str,
        local_images: &[PathBuf],
        lane: ComputeLane,
        compute: &ComputeConfig,
    ) -> Result<Vec<Value>> {
        let model = self.model_info(&compute.lane(lane).model)?;
        if !local_images.is_empty()
            && !model
                .input_modalities
                .iter()
                .any(|modality| modality == "image")
        {
            anyhow::bail!(
                "the configured {} model does not accept image input",
                lane.as_str()
            );
        }
        Ok(text_and_image_input_items(text, local_images))
    }

    fn model_display_name(&self, slug: &str) -> String {
        self.models
            .iter()
            .find(|model| model.model == slug || model.id == slug)
            .map(|model| model.display_name.clone())
            .unwrap_or_else(|| slug.to_owned())
    }
}

fn granular_approval_policy() -> Value {
    json!({
        "granular": {
            "mcp_elicitations": true,
            "request_permissions": true,
            "rules": true,
            "sandbox_approval": true,
            "skill_approval": true
        }
    })
}

fn text_input_items(text: &str) -> Vec<Value> {
    let text = text.trim();
    if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": text })]
    }
}

pub(super) fn text_and_image_input_items(text: &str, local_images: &[PathBuf]) -> Vec<Value> {
    let mut items = text_input_items(text);
    items.extend(local_images.iter().map(|path| {
        json!({
            "type": "localImage",
            "path": path,
            "detail": "auto"
        })
    }));
    items
}

impl TokenBreakdown {
    fn from_value(value: &Value) -> Self {
        Self {
            input_tokens: unsigned(value, "inputTokens"),
            cached_input_tokens: unsigned(value, "cachedInputTokens"),
            output_tokens: unsigned(value, "outputTokens"),
            reasoning_output_tokens: unsigned(value, "reasoningOutputTokens"),
            total_tokens: unsigned(value, "totalTokens"),
        }
    }

    fn difference(&self, baseline: &Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(baseline.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(baseline.cached_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(baseline.output_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_sub(baseline.reasoning_output_tokens),
            total_tokens: self.total_tokens.saturating_sub(baseline.total_tokens),
        }
    }
}

fn event_matches(params: &Value, thread_id: &str, turn_id: &Option<String>) -> bool {
    if params.get("threadId").and_then(Value::as_str) != Some(thread_id) {
        return false;
    }
    turn_id.as_ref().is_none_or(|expected| {
        params.get("turnId").and_then(Value::as_str) == Some(expected.as_str())
    })
}

fn completed_turn_matches(params: &Value, thread_id: &str, turn_id: &Option<String>) -> bool {
    if params.get("threadId").and_then(Value::as_str) != Some(thread_id) {
        return false;
    }
    turn_id.as_ref().is_none_or(|expected| {
        params.pointer("/turn/id").and_then(Value::as_str) == Some(expected.as_str())
    })
}

fn final_agent_message(params: &Value) -> Option<String> {
    params
        .pointer("/turn/items")
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn completed_response_text(params: &Value, streamed_text: &str) -> String {
    final_agent_message(params)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| streamed_text.to_owned())
        .trim()
        .to_owned()
}

fn is_superseded_autonomous_response(text: &str) -> bool {
    text.trim() == AUTONOMOUS_SUPERSEDED_MARKER
}

fn activity_label(item: Option<&Value>) -> Option<String> {
    let item = item?;
    match item.get("type").and_then(Value::as_str)? {
        "reasoning" => Some("正在思考".to_owned()),
        "webSearch" => Some("正在检索外部信息".to_owned()),
        "agentMessage" => Some("正在组织回复".to_owned()),
        "commandExecution" => Some("正在使用本地工具".to_owned()),
        "mcpToolCall" => Some("正在调用外部能力".to_owned()),
        "imageGeneration" => Some("正在生成图片".to_owned()),
        "dynamicToolCall" => {
            let tool = item
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .rsplit('.')
                .next()
                .unwrap_or_default();
            Some(
                match tool {
                    "describe" => "正在确认长期上下文能力",
                    "list_scopes" => "正在查看上下文范围",
                    "browse_index" => "正在浏览长期上下文索引",
                    "search_pages" => "正在搜索长期上下文",
                    "read_pages" => "正在读取长期上下文",
                    "write_summary" => "正在建立上下文索引",
                    "write_page" => "正在整理长期上下文",
                    "revise_page" => "正在更新长期上下文",
                    "consolidate_pages" => "正在收敛重复上下文",
                    "relate_pages" => "正在建立上下文关系",
                    "complete_orientation" => "正在整理初始画像",
                    "revise_orientation" => "正在修订长期画像",
                    "update_current_map" => "正在整理近期脉络",
                    "update_open_loops" => "正在整理未决问题",
                    "record_profile_review" => "正在审查长期画像",
                    "upsert_compute_policy" => "正在保存话题计算规则",
                    "remove_compute_policy" => "正在移除话题计算规则",
                    "fetch_url" => "正在读取指定网页",
                    "open_hunch" => "正在留下一个待探索的问题",
                    "revise_hunch" => "正在修订探索中的问题",
                    "retire_hunch" => "正在结束一个探索问题",
                    "acknowledge_hunch_feedback" => "正在吸收对探索问题的反馈",
                    "request_exploration" => "正在留下一个待查证的问题",
                    "escalate" => "正在判断是否需要深入处理",
                    _ => "正在调用 symbiont 工具",
                }
                .to_owned(),
            )
        }
        _ => None,
    }
}

fn invocation_record(
    thread_id: &str,
    turn_id: &str,
    lane: ComputeLane,
    origin: &str,
    lane_config: &LaneConfig,
    effective_model: &str,
    model_display_name: &str,
    started_at: &str,
    duration: Duration,
    usage: TokenBreakdown,
    trace_steps: Vec<ToolTraceStep>,
    context_snapshot: Option<ContextSnapshot>,
    trace_events: Vec<ExecutionTraceEvent>,
) -> InvocationRecord {
    let tool_calls = trace_steps
        .iter()
        .map(|step| format!("{}.{}", step.namespace, step.tool))
        .collect();
    InvocationRecord {
        id: turn_id.to_owned(),
        parent_id: None,
        thread_id: thread_id.to_owned(),
        turn_id: turn_id.to_owned(),
        origin: origin.to_owned(),
        lane: lane.as_str().to_owned(),
        requested_model: lane_config.model.clone(),
        effective_model: effective_model.to_owned(),
        model_display_name: model_display_name.to_owned(),
        effort: lane_config.effort.clone(),
        service_tier: lane_config.service_tier.clone(),
        started_at: started_at.to_owned(),
        completed_at: now(),
        duration_ms: duration.as_millis() as u64,
        status: "completed".to_owned(),
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        total_tokens: usage.total_tokens,
        tool_calls,
        produced_message: false,
        trace_steps,
        context_snapshot,
        trace_events,
    }
}

fn metadata_for(invocations: &[InvocationRecord], origin: &str) -> MessageMetadata {
    MessageMetadata {
        runs: invocations
            .iter()
            .map(|run| MessageRunMetadata {
                model: run.effective_model.clone(),
                display_name: run.model_display_name.clone(),
                effort: run.effort.clone(),
                lane: run.lane.clone(),
                total_tokens: run.total_tokens,
                duration_ms: run.duration_ms,
            })
            .collect(),
        total_tokens: invocations.iter().map(|run| run.total_tokens).sum(),
        duration_ms: invocations.iter().map(|run| run.duration_ms).sum(),
        tool_calls: invocations
            .iter()
            .map(|run| run.tool_calls.len() as u64)
            .sum(),
        pcp_tool_calls: invocations
            .iter()
            .flat_map(|run| &run.tool_calls)
            .filter(|tool| tool.starts_with("pcp."))
            .count() as u64,
        trace_id: invocations.first().map(|run| run.id.clone()),
        origin: Some(origin.to_owned()),
    }
}

fn interrupted_chat_outcome(invocations: Vec<InvocationRecord>, origin: &str) -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        disposition: ChatDisposition::Reply,
        generated_images: Vec::new(),
        metadata: metadata_for(&invocations, origin),
        context_revision_ids: context_revision_ids(&invocations),
        scheduled_follow_up_ids: successful_tool_result_ids(
            &invocations,
            "symbiont",
            "schedule_follow_up",
            "id",
        ),
        reserved_continuation_ids: successful_tool_result_ids(
            &invocations,
            "symbiont",
            "reserve_continuation",
            "id",
        ),
        requested_exploration_ids: successful_tool_result_ids(
            &invocations,
            "symbiont",
            "request_exploration",
            "id",
        ),
        invocations,
        interrupted: true,
    }
}

pub(super) fn generated_image_output(item: &Value) -> Option<GeneratedImageOutput> {
    if item.get("type").and_then(Value::as_str) != Some("imageGeneration") {
        return None;
    }
    let saved_path = PathBuf::from(item.get("savedPath")?.as_str()?);
    if !saved_path.is_absolute() {
        return None;
    }
    Some(GeneratedImageOutput {
        item_id: item.get("id")?.as_str()?.to_owned(),
        saved_path,
        revised_prompt: item
            .get("revisedPrompt")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub(super) fn remember_generated_image(
    generated_images: &mut Vec<GeneratedImageOutput>,
    image: GeneratedImageOutput,
) {
    if generated_images
        .iter()
        .any(|existing| existing.saved_path == image.saved_path)
    {
        return;
    }
    generated_images.push(image);
}

fn successful_hunch_revisions(invocations: &[InvocationRecord]) -> Vec<HunchRevisionRef> {
    let mut revisions = Vec::<HunchRevisionRef>::new();
    for step in invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .filter(|step| {
            step.succeeded
                && step.namespace == "symbiont"
                && matches!(
                    step.tool.as_str(),
                    "open_hunch" | "revise_hunch" | "retire_hunch"
                )
        })
    {
        let Some(text) = step
            .result
            .pointer("/contentItems/0/text")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let (Some(page_id), Some(revision_id)) = (
            value.get("pageId").and_then(Value::as_str),
            value.get("revisionId").and_then(Value::as_str),
        ) else {
            continue;
        };
        let reference = HunchRevisionRef {
            page_id: page_id.to_owned(),
            revision_id: revision_id.to_owned(),
        };
        if let Some(existing) = revisions
            .iter_mut()
            .find(|existing| existing.page_id == reference.page_id)
        {
            *existing = reference;
        } else {
            revisions.push(reference);
        }
    }
    revisions
}

fn reconciliation_actions(invocations: &[InvocationRecord]) -> Vec<ReconciliationAction> {
    invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .filter(|step| {
            step.succeeded
                && step.namespace == "pcp"
                && matches!(
                    step.tool.as_str(),
                    "assess_validity"
                        | "write_summary"
                        | "write_page"
                        | "revise_page"
                        | "consolidate_pages"
                        | "relate_pages"
                )
        })
        .map(|step| {
            let mut target_revision_ids = HashSet::new();
            collect_revision_ids(&step.arguments, &mut target_revision_ids);
            let result = step
                .result
                .pointer("/contentItems/0/text")
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or(Value::Null);
            ReconciliationAction {
                tool: format!("{}.{}", step.namespace, step.tool),
                target_revision_ids: {
                    let mut values = target_revision_ids.into_iter().collect::<Vec<_>>();
                    values.sort();
                    values
                },
                result_page_id: result
                    .get("pageId")
                    .or_else(|| result.get("summaryPageId"))
                    .or_else(|| result.get("assessmentPageId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                result_revision_id: result
                    .get("revisionId")
                    .or_else(|| result.get("summaryRevisionId"))
                    .or_else(|| result.get("assessmentRevisionId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                result_relation_id: result
                    .get("relationId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
        .collect()
}

fn proactive_message_candidate(invocations: &[InvocationRecord]) -> Option<OutreachCandidate> {
    invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .rev()
        .find(|step| {
            step.succeeded && step.namespace == "symbiont" && step.tool == PROPOSE_OUTREACH_TOOL
        })
        .and_then(|step| OutreachCandidate::from_tool_arguments(&step.arguments))
}

fn successful_tool_result_ids(
    invocations: &[InvocationRecord],
    namespace: &str,
    tool: &str,
    field: &str,
) -> Vec<String> {
    let mut ids = invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .filter(|step| step.succeeded && step.namespace == namespace && step.tool == tool)
        .filter_map(|step| {
            step.result
                .pointer("/contentItems/0/text")
                .and_then(Value::as_str)
        })
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .filter_map(|value| value.get(field).and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn invocation_wrote_checkpoint(invocation: &InvocationRecord) -> bool {
    invocation.trace_steps.iter().any(|step| {
        step.namespace == "pcp"
            && step.tool == "write_page"
            && step
                .arguments
                .pointer("/facets/kind")
                .and_then(Value::as_str)
                == Some("conversation_checkpoint")
    })
}

fn successful_symbiont_tool(invocations: &[InvocationRecord], tool: &str) -> bool {
    invocations.iter().any(|invocation| {
        invocation
            .trace_steps
            .iter()
            .any(|step| step.namespace == "symbiont" && step.tool == tool && step.succeeded)
    })
}

pub(super) fn context_revision_ids(invocations: &[InvocationRecord]) -> Vec<String> {
    let mut revisions = HashSet::new();
    for step in invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .filter(|step| step.namespace == "pcp" && step.succeeded)
    {
        collect_revision_ids(&step.arguments, &mut revisions);
        if let Some(text) = step
            .result
            .pointer("/contentItems/0/text")
            .and_then(Value::as_str)
            && let Ok(value) = serde_json::from_str::<Value>(text)
        {
            collect_revision_ids(&value, &mut revisions);
        }
    }
    let mut revisions = revisions.into_iter().collect::<Vec<_>>();
    revisions.sort();
    revisions
}

fn collect_revision_ids(value: &Value, revisions: &mut HashSet<String>) {
    match value {
        Value::String(value) if is_canonical_page_id(value) => {
            revisions.insert(value.clone());
        }
        Value::Array(values) => {
            for value in values {
                collect_revision_ids(value, revisions);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_revision_ids(value, revisions);
            }
        }
        _ => {}
    }
}

fn is_canonical_page_id(value: &str) -> bool {
    ["rev_", "sumrev_", "valid_"]
        .into_iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn normalize_trace_arguments(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "value": text }))
        }
        Some(value) => value.clone(),
        None => json!({}),
    }
}

async fn send_event(events: &mpsc::Sender<RuntimeEvent>, event: RuntimeEvent) {
    let _ = events.send(event).await;
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn unsigned(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
}

fn optional_text(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown app-server error")
        .to_owned()
}

#[cfg(test)]
pub(super) fn extract_final_agent_message(params: &Value) -> Option<String> {
    final_agent_message(params)
}

#[cfg(test)]
pub(super) fn extract_completed_response_text(params: &Value, streamed_text: &str) -> String {
    completed_response_text(params, streamed_text)
}

#[cfg(test)]
pub(super) fn autonomous_response_is_superseded(text: &str) -> bool {
    is_superseded_autonomous_response(text)
}
