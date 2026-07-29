use std::{convert::Infallible, sync::Arc};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Multipart, Path as AxumPath, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pcp_core::{
    Projection, ReadPage, ReadPagesRequest, Scope, SearchFilters, SearchMode, SearchPagesRequest,
};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, RwLock, mpsc},
    task::JoinHandle,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use crate::{
    asset::{AssetStore, MAX_IMAGE_BYTES, MAX_IMAGES_PER_MESSAGE, SavedImage},
    autonomy::{AutonomyConfig, AutonomyStore},
    codex::{ChatInput, CodexClient, RateLimitInfo, RuntimeEvent},
    compute::{ComputeConfig, ComputeStore, ModelInfo},
    continuity::{ContinuityHost, MessageLinks},
    exploration::{ExplorationHandle, ExplorationSnapshot, today_started_at},
    memory::{MemoryEntry, MemoryRole},
    profile::{CalibrationMode, ProfileSnapshot, ProfileStore, SetupStatus},
    usage::{TraceBundle, UsageHeadline, UsageStore, UsageSummary},
};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const RICH_TEXT_JS: &str = include_str!("../web/rich-text.js");
const RICH_TEXT_CSS: &str = include_str!("../web/rich-text.css");
const PRESENTATION_JS: &str = include_str!("../web/presentation.js");
const PROFILE_UI_JS: &str = include_str!("../web/profile-ui.js");
const SETTINGS_JS: &str = include_str!("../web/settings.js");
const TRACE_UI_JS: &str = include_str!("../web/trace-ui.js");
const STYLES_CSS: &str = include_str!("../web/styles.css");
const MAX_USER_MESSAGE_CHARS: usize = 12_000;
const MAX_CHAT_BODY_BYTES: usize =
    MAX_USER_MESSAGE_CHARS + (MAX_IMAGE_BYTES * MAX_IMAGES_PER_MESSAGE) + 64_000;

#[derive(Clone)]
pub struct AppState {
    continuity: Arc<ContinuityHost>,
    assets: Arc<AssetStore>,
    profile: Arc<ProfileStore>,
    autonomy: Arc<AutonomyStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    usage: Arc<UsageStore>,
    rate_limits: Arc<RwLock<Option<RateLimitInfo>>>,
    exploration: ExplorationHandle,
}

impl AppState {
    pub fn new(
        continuity: Arc<ContinuityHost>,
        assets: Arc<AssetStore>,
        profile: Arc<ProfileStore>,
        autonomy: Arc<AutonomyStore>,
        codex: Arc<Mutex<CodexClient>>,
        compute: Arc<ComputeStore>,
        usage: Arc<UsageStore>,
        rate_limits: Arc<RwLock<Option<RateLimitInfo>>>,
        exploration: ExplorationHandle,
    ) -> Self {
        Self {
            continuity,
            assets,
            profile,
            autonomy,
            codex,
            compute,
            usage,
            rate_limits,
            exploration,
        }
    }
}

struct ChatRequest {
    message: String,
    images: Vec<SavedImage>,
}

struct IncomingChatRequest {
    message: String,
    images: Vec<(Option<String>, Bytes)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapResponse {
    messages: Vec<MemoryEntry>,
    memory_chars: usize,
    status: &'static str,
    profile: ProfileSnapshot,
    autonomy: AutonomyConfig,
    autonomy_permitted: bool,
    models: Vec<ModelInfo>,
    compute: ComputeConfig,
    rate_limits: Option<RateLimitInfo>,
    usage: UsageHeadline,
    exploration: ExplorationSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsResponse {
    usage: UsageSummary,
    headline: UsageHeadline,
    rate_limits: Option<RateLimitInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeResponse {
    usage: UsageHeadline,
    exploration: ExplorationSnapshot,
}

#[derive(Serialize)]
struct TriggerResponse {
    accepted: bool,
}

#[derive(Deserialize)]
struct OnboardingRequest {
    mode: CalibrationMode,
}

#[derive(Deserialize)]
struct OrientationRequest {
    orientation: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveResponse {
    profile: ProfileSnapshot,
    memory: Vec<MemoryEntry>,
    memory_chars: usize,
    autonomy_permitted: bool,
    pcp: PcpArchiveResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PcpArchiveResponse {
    scopes: Vec<Scope>,
    pages: Vec<ReadPage>,
    page_count: u64,
}

#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum WireEvent {
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
    Complete {
        message: MemoryEntry,
        memory_chars: usize,
        profile: ProfileSnapshot,
        autonomy_permitted: bool,
        usage: UsageHeadline,
        exploration: ExplorationSnapshot,
    },
    Error {
        error: String,
    },
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/rich-text.js", get(rich_text_js))
        .route("/rich-text.css", get(rich_text_css))
        .route("/presentation.js", get(presentation_js))
        .route("/profile-ui.js", get(profile_ui_js))
        .route("/settings.js", get(settings_js))
        .route("/trace-ui.js", get(trace_ui_js))
        .route("/styles.css", get(styles_css))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/chat", post(chat))
        .route("/api/assets/{asset_id}", get(asset))
        .route("/api/onboarding/start", post(start_onboarding))
        .route("/api/archive", get(archive))
        .route("/api/profile/orientation", post(update_orientation))
        .route("/api/autonomy", post(update_autonomy))
        .route("/api/exploration/run", post(trigger_exploration))
        .route("/api/compute", post(update_compute))
        .route("/api/stats", get(stats))
        .route("/api/runtime", get(runtime))
        .route("/api/traces/{trace_id}", get(trace_detail))
        .layer(DefaultBodyLimit::max(MAX_CHAT_BODY_BYTES))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn rich_text_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        RICH_TEXT_JS,
    )
}

async fn rich_text_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        RICH_TEXT_CSS,
    )
}

async fn presentation_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        PRESENTATION_JS,
    )
}

async fn profile_ui_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        PROFILE_UI_JS,
    )
}

async fn settings_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        SETTINGS_JS,
    )
}

async fn trace_ui_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        TRACE_UI_JS,
    )
}

async fn styles_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

async fn bootstrap(State(state): State<AppState>) -> Result<Json<BootstrapResponse>, ApiError> {
    let messages = state
        .continuity
        .recent_messages(100)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .filter(|entry| matches!(entry.role, MemoryRole::User | MemoryRole::Assistant))
        .collect();
    let memory_chars = state
        .continuity
        .memory_chars()
        .await
        .map_err(ApiError::internal)?;
    let profile = state.profile.snapshot().await;
    let autonomy = state.autonomy.snapshot().await;
    let autonomy_permitted = state
        .autonomy
        .permitted(profile.status == SetupStatus::Ready)
        .await;
    let usage = state
        .usage
        .headline(&today_started_at())
        .await
        .map_err(ApiError::internal)?;
    let exploration = state.exploration.snapshot().await;

    Ok(Json(BootstrapResponse {
        messages,
        memory_chars,
        status: "connected",
        profile,
        autonomy,
        autonomy_permitted,
        models: state.compute.catalog().to_vec(),
        compute: state.compute.snapshot().await,
        rate_limits: state.rate_limits.read().await.clone(),
        usage,
        exploration,
    }))
}

async fn start_onboarding(
    State(state): State<AppState>,
    Json(request): Json<OnboardingRequest>,
) -> Result<Json<ProfileSnapshot>, ApiError> {
    state
        .profile
        .begin(request.mode)
        .await
        .map(Json)
        .map_err(ApiError::conflict)
}

async fn archive(State(state): State<AppState>) -> Result<Json<ArchiveResponse>, ApiError> {
    let profile = state.profile.snapshot().await;
    let memory = state
        .continuity
        .recent_messages(500)
        .await
        .map_err(ApiError::internal)?;
    let memory_chars = state
        .continuity
        .memory_chars()
        .await
        .map_err(ApiError::internal)?;
    let autonomy_permitted = state
        .autonomy
        .permitted(profile.status == SetupStatus::Ready)
        .await;
    let (scopes, _) = state
        .continuity
        .list_scopes(None, 100, None)
        .await
        .map_err(ApiError::internal)?;
    let page_count = state
        .continuity
        .store()
        .page_count(state.continuity.allowed_scopes())
        .await
        .map_err(ApiError::internal)?;
    let recent_pages = state
        .continuity
        .search(SearchPagesRequest {
            query: String::new(),
            scopes: Vec::new(),
            mode: SearchMode::Temporal,
            filters: SearchFilters::default(),
            limit: 20,
            cursor: None,
        })
        .await
        .map_err(ApiError::internal)?;
    let pages = state
        .continuity
        .read(ReadPagesRequest {
            revision_ids: recent_pages
                .hits
                .into_iter()
                .map(|hit| hit.revision_id)
                .collect(),
            projections: vec![
                Projection::Manifest,
                Projection::Payload,
                Projection::Sources,
                Projection::Provenance,
                Projection::Facets,
                Projection::Relations,
                Projection::History,
            ],
            max_chars: 64_000,
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(ArchiveResponse {
        profile,
        memory,
        memory_chars,
        autonomy_permitted,
        pcp: PcpArchiveResponse {
            scopes,
            pages,
            page_count,
        },
    }))
}

async fn update_orientation(
    State(state): State<AppState>,
    Json(request): Json<OrientationRequest>,
) -> Result<Json<ProfileSnapshot>, ApiError> {
    let profile = state
        .profile
        .update_orientation(&request.orientation)
        .await
        .map_err(ApiError::bad_request)?;
    let sources = state
        .continuity
        .recent_source_revisions(20)
        .await
        .map_err(ApiError::internal)?;
    state
        .continuity
        .sync_orientation(&profile, sources)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(profile))
}

async fn update_autonomy(
    State(state): State<AppState>,
    Json(config): Json<AutonomyConfig>,
) -> Result<Json<AutonomyConfig>, ApiError> {
    state
        .autonomy
        .update(config)
        .await
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn update_compute(
    State(state): State<AppState>,
    Json(config): Json<ComputeConfig>,
) -> Result<Json<ComputeConfig>, ApiError> {
    state
        .compute
        .update(config)
        .await
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn stats(State(state): State<AppState>) -> Result<Json<StatsResponse>, ApiError> {
    let usage = state.usage.summary().await.map_err(ApiError::internal)?;
    let headline = state
        .usage
        .headline(&today_started_at())
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(StatsResponse {
        usage,
        headline,
        rate_limits: state.rate_limits.read().await.clone(),
    }))
}

async fn runtime(State(state): State<AppState>) -> Result<Json<RuntimeResponse>, ApiError> {
    let usage = state
        .usage
        .headline(&today_started_at())
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(RuntimeResponse {
        usage,
        exploration: state.exploration.snapshot().await,
    }))
}

async fn trigger_exploration(
    State(state): State<AppState>,
) -> Result<Json<TriggerResponse>, ApiError> {
    let profile = state.profile.snapshot().await;
    if !state
        .autonomy
        .permitted(profile.status == SetupStatus::Ready)
        .await
    {
        return Err(ApiError::conflict(
            "Autonomous exploration is not currently enabled.",
        ));
    }
    let accepted = state.exploration.trigger();
    if !accepted {
        return Err(ApiError::conflict(
            "An exploration request is already queued.",
        ));
    }
    Ok(Json(TriggerResponse { accepted }))
}

async fn trace_detail(
    State(state): State<AppState>,
    AxumPath(trace_id): AxumPath<String>,
) -> Result<Json<TraceBundle>, ApiError> {
    state
        .usage
        .trace(&trace_id)
        .await
        .map_err(ApiError::internal)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("Invocation trace is not available."))
}

async fn asset(
    State(state): State<AppState>,
    AxumPath(asset_id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let (bytes, mime_type) = state
        .assets
        .read(&asset_id)
        .await
        .map_err(|_| ApiError::not_found("Image asset is not available."))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, mime_type)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .expect("valid image asset response"))
}

async fn chat(State(state): State<AppState>, multipart: Multipart) -> Result<Response, ApiError> {
    let profile = state.profile.snapshot().await;
    if profile.status == SetupStatus::Unconfigured {
        return Err(ApiError::conflict(
            "Start the initial conversation before sending a message.",
        ));
    }
    let incoming = parse_chat_request(multipart).await?;
    let message = incoming.message.trim().to_owned();
    if message.is_empty() && incoming.images.is_empty() {
        return Err(ApiError::bad_request(
            "A message requires text or an image.",
        ));
    }
    if message.chars().count() > MAX_USER_MESSAGE_CHARS {
        return Err(ApiError::bad_request(format!(
            "Message exceeds {MAX_USER_MESSAGE_CHARS} characters."
        )));
    }
    let mut images = Vec::with_capacity(incoming.images.len());
    for (filename, bytes) in incoming.images {
        images.push(
            state
                .assets
                .save_image(filename.as_deref(), &bytes)
                .await
                .map_err(ApiError::bad_request)?,
        );
    }
    let request = ChatRequest { message, images };

    let (wire_tx, wire_rx) = mpsc::channel::<WireEvent>(64);
    let (runtime_tx, mut runtime_rx) = mpsc::channel::<RuntimeEvent>(64);
    let runtime_wire_tx = wire_tx.clone();
    let runtime_forwarder = tokio::spawn(async move {
        while let Some(event) = runtime_rx.recv().await {
            if runtime_wire_tx.send(event.into()).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(async move {
        if let Err(error) = run_chat(
            state,
            request,
            runtime_tx,
            runtime_forwarder,
            wire_tx.clone(),
        )
        .await
        {
            tracing::error!(%error, "chat stream failed");
            let _ = wire_tx
                .send(WireEvent::Error {
                    error: error.to_string(),
                })
                .await;
        }
    });

    let stream = ReceiverStream::new(wire_rx).map(|event| {
        let mut line = serde_json::to_vec(&event).unwrap_or_else(|error| {
            format!(r#"{{"type":"error","error":"encode stream event: {error}"}}"#).into_bytes()
        });
        line.push(b'\n');
        Ok::<Bytes, Infallible>(Bytes::from(line))
    });

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .expect("valid streaming response"))
}

async fn parse_chat_request(mut multipart: Multipart) -> Result<IncomingChatRequest, ApiError> {
    let mut message = String::new();
    let mut images = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("Invalid message body: {error}")))?
    {
        match field.name() {
            Some("message") => {
                message = field
                    .text()
                    .await
                    .map_err(|error| ApiError::bad_request(format!("Invalid message: {error}")))?;
            }
            Some("image") => {
                if images.len() >= MAX_IMAGES_PER_MESSAGE {
                    return Err(ApiError::bad_request(format!(
                        "A message can contain at most {MAX_IMAGES_PER_MESSAGE} images."
                    )));
                }
                let filename = field.file_name().map(str::to_owned);
                let bytes = field.bytes().await.map_err(|error| {
                    ApiError::bad_request(format!("Could not read image: {error}"))
                })?;
                images.push((filename, bytes));
            }
            _ => {}
        }
    }
    Ok(IncomingChatRequest { message, images })
}

async fn run_chat(
    state: AppState,
    request: ChatRequest,
    runtime_tx: mpsc::Sender<RuntimeEvent>,
    runtime_forwarder: JoinHandle<()>,
    wire_tx: mpsc::Sender<WireEvent>,
) -> anyhow::Result<()> {
    let image_paths = request
        .images
        .iter()
        .map(|image| image.path.clone())
        .collect();
    let user_message = state
        .continuity
        .ingest_message(
            MemoryRole::User,
            &request.message,
            request.images,
            None,
            MessageLinks::default(),
        )
        .await?;
    let compute = state.compute.snapshot().await;
    let profile = state.profile.snapshot().await;
    let continuity_context = state.continuity.context_seed(Some(&user_message)).await;
    let outcome = state
        .codex
        .lock()
        .await
        .chat(
            ChatInput {
                text: request.message,
                local_images: image_paths,
            },
            &compute,
            &profile,
            &continuity_context,
            runtime_tx,
        )
        .await;
    runtime_forwarder.await?;
    let outcome = outcome?;
    state.usage.record_all(&outcome.invocations).await?;
    let mut input_revision_ids = user_message.attachment_revision_ids;
    input_revision_ids.extend(outcome.context_revision_ids);
    input_revision_ids.sort();
    input_revision_ids.dedup();
    let message = state
        .continuity
        .ingest_message(
            MemoryRole::Assistant,
            &outcome.text,
            Vec::new(),
            Some(outcome.metadata),
            MessageLinks {
                responds_to: Some(user_message.page.revision_id.clone()),
                input_revision_ids,
            },
        )
        .await?
        .entry;
    let memory_chars = state.continuity.memory_chars().await?;
    let profile = state.profile.snapshot().await;
    let autonomy_permitted = state
        .autonomy
        .permitted(profile.status == SetupStatus::Ready)
        .await;
    let usage = state.usage.headline(&today_started_at()).await?;
    let exploration = state.exploration.snapshot().await;
    let _ = wire_tx
        .send(WireEvent::Complete {
            message,
            memory_chars,
            profile,
            autonomy_permitted,
            usage,
            exploration,
        })
        .await;
    Ok(())
}

impl From<RuntimeEvent> for WireEvent {
    fn from(event: RuntimeEvent) -> Self {
        match event {
            RuntimeEvent::Activity {
                label,
                model,
                display_name,
                effort,
                lane,
            } => Self::Activity {
                label,
                model,
                display_name,
                effort,
                lane,
            },
            RuntimeEvent::Delta { text } => Self::Delta { text },
            RuntimeEvent::Reset => Self::Reset,
        }
    }
}

impl ApiError {
    fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }

    fn not_found(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: error.to_string(),
        }
    }

    fn conflict(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}
