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
    sync::{RwLock, mpsc},
    time::{sleep, timeout},
};
use tracing::{debug, warn};

use super::{
    prompts::{
        additional_context_value, autonomous_exploration_prompt, context_fragments,
        context_maintenance_prompt, developer_instructions, profile_review_prompt,
        summary_maintenance_prompt,
    },
    tool_dedup::{ToolCallPlan, TurnToolDeduplicator},
    tools::{EscalationRequest, SymbiontTools, tool_result},
    trace::{
        observable_history_item, observable_item_event, push_trace_event, timestamp_from_millis,
    },
};
use crate::{
    compute::{ComputeConfig, ComputeLane, LaneConfig, ModelInfo},
    continuity::ContinuityHost,
    curiosity::CuriosityStore,
    diagnostics::{ContextSnapshot, ExecutionTraceEvent, NativeThreadSnapshot, TraceEventKind},
    memory::{MessageMetadata, MessageRunMetadata},
    profile::{ProfileSnapshot, ProfileStore},
    rollover::{self, NativeThreadCursor, RolloverDecision, ThreadContextPressure},
    symbiont_context::SymbiontContextStore,
    usage::{InvocationRecord, ToolTraceStep},
    working_context::WorkingContext,
};

const AUTONOMOUS_SILENT_MARKER: &str = "<symbiont-silent/>";
const MAINTENANCE_COMPLETE_MARKER: &str = "<symbiont-maintained/>";
const CONTEXT_MAINTENANCE_COMPLETE_MARKER: &str = "<symbiont-context-maintained/>";
const PROFILE_REVIEW_COMPLETE_MARKER: &str = "<symbiont-profile-reviewed/>";

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
    pub metadata: MessageMetadata,
    pub invocations: Vec<InvocationRecord>,
    pub context_revision_ids: Vec<String>,
    pub hunch_touched: bool,
}

pub struct ExplorationOutcome {
    pub message: Option<String>,
    pub metadata: MessageMetadata,
    pub invocations: Vec<InvocationRecord>,
    pub context_revision_ids: Vec<String>,
}

pub struct MaintenanceOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub summarized: bool,
    pub model: Option<String>,
}

pub struct ContextMaintenanceOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub current_map_updated: bool,
    pub open_loops_updated: bool,
}

pub struct ProfileReviewOutcome {
    pub invocations: Vec<InvocationRecord>,
    pub status: Option<String>,
    pub clarification_question: Option<String>,
    pub metadata: MessageMetadata,
    pub context_revision_ids: Vec<String>,
}

pub struct ChatInput {
    pub text: String,
    pub local_images: Vec<PathBuf>,
    pub current_revision_id: String,
    pub reply_to_revision_id: Option<String>,
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
    invocation: InvocationRecord,
    escalation: Option<EscalationRequest>,
}

#[derive(Clone, Copy)]
enum BackgroundThread {
    Autonomous,
    Maintenance,
}

pub struct CodexClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    interactive_thread_id: String,
    autonomous_thread_id: String,
    maintenance_thread_id: String,
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
}

impl CodexClient {
    pub async fn start(
        config: CodexConfig,
        continuity: Arc<ContinuityHost>,
        profile: Arc<ProfileStore>,
        context: Arc<SymbiontContextStore>,
        curiosity: Arc<CuriosityStore>,
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
            autonomous_thread_id: String::new(),
            maintenance_thread_id: String::new(),
            workspace: config.workspace.clone(),
            continuity: Arc::clone(&continuity),
            interactive_cursor: NativeThreadCursor::new(),
            tools: SymbiontTools::new(continuity, profile, context, curiosity),
            models: Vec::new(),
            thread_usage: HashMap::new(),
            thread_turns: HashMap::new(),
            thread_compactions: HashMap::new(),
            thread_context_pressure: HashMap::new(),
            rate_limits: Arc::new(RwLock::new(None)),
        };
        client.initialize().await?;
        client.models = client.load_models().await?;
        client.refresh_rate_limits().await;
        client.interactive_thread_id = client.start_thread(&config.workspace).await?;
        client.autonomous_thread_id = client.start_thread(&config.workspace).await?;
        client.maintenance_thread_id = client.start_thread(&config.workspace).await?;
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
        let first_lane = ComputeLane::Conversation;
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
                true,
                &events,
            )
            .await?;
        if needs_bridge {
            self.interactive_cursor.bridge_completed();
        }
        if let Some(rollover) = rollover {
            let workspace = self.workspace.clone();
            match self.start_thread(&workspace).await {
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

    pub fn mark_interactive_revision(&mut self, revision_id: String) {
        self.interactive_cursor.mark(revision_id);
    }

    pub async fn explore(
        &mut self,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<ExplorationOutcome> {
        let prompt = autonomous_exploration_prompt(AUTONOMOUS_SILENT_MARKER);
        let thread_id = self.autonomous_thread_id.clone();
        let outcome = self
            .run_request(
                thread_id.clone(),
                text_input_items(&prompt),
                ComputeLane::Observe,
                "autonomous",
                compute,
                profile,
                continuity_context,
                None,
                None,
                true,
                &events,
            )
            .await;
        self.renew_background_thread(&thread_id, BackgroundThread::Autonomous)
            .await;
        let mut outcome = outcome?;
        let message = if is_silent_autonomous_response(&outcome.text) {
            if let Some(last) = outcome.invocations.last_mut() {
                last.produced_message = false;
            }
            None
        } else {
            Some(outcome.text)
        };
        outcome.metadata = metadata_for(&outcome.invocations, "autonomous");
        Ok(ExplorationOutcome {
            message,
            metadata: outcome.metadata,
            invocations: outcome.invocations,
            context_revision_ids: outcome.context_revision_ids,
        })
    }

    pub async fn maintain_summary(
        &mut self,
        target_revision_id: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
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
                &events,
            )
            .await;
        self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
            .await;
        let mut outcome = outcome?;
        for invocation in &mut outcome.invocations {
            invocation.produced_message = false;
        }
        let summarized = outcome.invocations.iter().any(|invocation| {
            invocation.trace_steps.iter().any(|step| {
                step.namespace == "pcp"
                    && step.tool == "write_summary"
                    && step.succeeded
                    && step
                        .arguments
                        .get("target_revision_id")
                        .and_then(Value::as_str)
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
        })
    }

    pub async fn maintain_symbiont_context(
        &mut self,
        source_bundle: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
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
                &events,
            )
            .await;
        self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
            .await;
        let mut outcome = outcome?;
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
        })
    }

    pub async fn review_profile(
        &mut self,
        source_bundle: &str,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        continuity_context: &str,
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
                &events,
            )
            .await;
        self.renew_background_thread(&thread_id, BackgroundThread::Maintenance)
            .await;
        let mut outcome = outcome?;
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
        })
    }

    async fn renew_background_thread(&mut self, previous: &str, slot: BackgroundThread) {
        let workspace = self.workspace.clone();
        match self.start_thread(&workspace).await {
            Ok(next) => {
                match slot {
                    BackgroundThread::Autonomous => self.autonomous_thread_id = next,
                    BackgroundThread::Maintenance => self.maintenance_thread_id = next,
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
                events,
            )
            .await?;
        let root_id = first.invocation.id.clone();
        let mut invocations = Vec::new();

        let final_text = if let Some(escalation) = first.escalation.take() {
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
                        events,
                    )
                    .await?;
                deep.invocation.parent_id = Some(root_id);
                deep.invocation.produced_message = true;
                let text = deep.text;
                invocations.push(deep.invocation);
                text
            } else {
                first.invocation.produced_message = true;
                let text = first.text;
                invocations.push(first.invocation);
                text
            }
        } else {
            first.invocation.produced_message = true;
            let text = first.text;
            invocations.push(first.invocation);
            text
        };

        let metadata = metadata_for(&invocations, origin);
        let context_revision_ids = context_revision_ids(&invocations);
        let hunch_touched = hunch_was_touched(&invocations);
        Ok(ChatOutcome {
            text: final_text,
            metadata,
            invocations,
            context_revision_ids,
            hunch_touched,
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
        let fragments = context_fragments(
            lane,
            allow_escalation,
            profile,
            continuity_context,
            working_context,
            rollover,
        );
        let mut context_snapshot = ContextSnapshot {
            input: input.clone(),
            fragments: fragments.clone(),
            working_context: working_context.cloned(),
            developer_instructions: developer_instructions(),
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

        let request_id = self
            .send_request("turn/start", Value::Object(params))
            .await?;
        let mut turn_id: Option<String> = None;
        let mut response_text = String::new();
        let mut escalation = None;
        let mut trace_steps = Vec::new();
        let mut trace_events = Vec::new();
        let mut tool_deduplicator = TurnToolDeduplicator::default();
        let mut effective_model = lane_config.model.clone();

        loop {
            let message = self.read_message().await?;
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
                        send_event(
                            events,
                            RuntimeEvent::Delta {
                                text: delta.to_owned(),
                            },
                        )
                        .await;
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
                    let status = params
                        .pointer("/turn/status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    if status != "completed" {
                        let error = params
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("Codex turn did not complete");
                        anyhow::bail!("{error}");
                    }
                    let response_text = completed_response_text(params, &response_text);
                    if response_text.is_empty() {
                        anyhow::bail!("Codex completed without an assistant message");
                    }
                    push_trace_event(
                        &mut trace_events,
                        TraceEventKind::AgentMessage,
                        "Final assistant message",
                        json!({"text": response_text.clone()}),
                        now(),
                    );
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
                    let invocation = invocation_record(
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
                    return Ok(TurnOutcome {
                        text: response_text,
                        invocation,
                        escalation,
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

    async fn start_thread(&mut self, workspace: &PathBuf) -> Result<String> {
        let instructions = developer_instructions();
        let result = self
            .request(
                "thread/start",
                json!({
                    "cwd": workspace,
                    "approvalPolicy": "never",
                    "sandbox": "read-only",
                    "ephemeral": true,
                    "serviceName": "symbiont-d",
                    "developerInstructions": instructions,
                    "config": {
                        "web_search": "live"
                    },
                    "dynamicTools": SymbiontTools::specifications()
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
        let model = self.model_info(&compute.lane(lane).model)?;
        if !input.local_images.is_empty()
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
        Ok(multimodal_input_items(input))
    }

    fn model_display_name(&self, slug: &str) -> String {
        self.models
            .iter()
            .find(|model| model.model == slug || model.id == slug)
            .map(|model| model.display_name.clone())
            .unwrap_or_else(|| slug.to_owned())
    }
}

fn text_input_items(text: &str) -> Vec<Value> {
    let text = text.trim();
    if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": text })]
    }
}

pub(super) fn multimodal_input_items(input: &ChatInput) -> Vec<Value> {
    let mut items = text_input_items(&input.text);
    items.extend(input.local_images.iter().map(|path| {
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

fn is_silent_autonomous_response(text: &str) -> bool {
    text.trim() == AUTONOMOUS_SILENT_MARKER
}

fn activity_label(item: Option<&Value>) -> Option<String> {
    let item = item?;
    match item.get("type").and_then(Value::as_str)? {
        "reasoning" => Some("正在思考".to_owned()),
        "webSearch" => Some("正在检索外部信息".to_owned()),
        "agentMessage" => Some("正在组织回复".to_owned()),
        "commandExecution" => Some("正在使用本地工具".to_owned()),
        "mcpToolCall" => Some("正在调用外部能力".to_owned()),
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
                    "search_pages" => "正在搜索长期上下文",
                    "read_pages" => "正在读取长期上下文",
                    "write_summary" => "正在建立上下文索引",
                    "write_page" => "正在整理长期上下文",
                    "revise_page" => "正在修订长期上下文",
                    "link_pages" => "正在建立上下文关系",
                    "complete_orientation" => "正在整理初始画像",
                    "revise_orientation" => "正在修订长期画像",
                    "update_current_map" => "正在整理近期脉络",
                    "update_open_loops" => "正在整理未决问题",
                    "record_profile_review" => "正在审查长期画像",
                    "open_hunch" => "正在留下一个待探索的问题",
                    "revise_hunch" => "正在修订探索中的问题",
                    "retire_hunch" => "正在结束一个探索问题",
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

fn hunch_was_touched(invocations: &[InvocationRecord]) -> bool {
    invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .any(|step| {
            step.succeeded
                && step.namespace == "symbiont"
                && matches!(step.tool.as_str(), "open_hunch" | "revise_hunch")
        })
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

fn context_revision_ids(invocations: &[InvocationRecord]) -> Vec<String> {
    let mut revisions = HashSet::new();
    for step in invocations
        .iter()
        .flat_map(|invocation| &invocation.trace_steps)
        .filter(|step| step.namespace == "pcp")
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
        Value::String(value) if value.starts_with("rev_") => {
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
pub(super) fn autonomous_response_is_silent(text: &str) -> bool {
    is_silent_autonomous_response(text)
}
