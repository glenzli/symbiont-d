//! Shared no-daemon fixture server for Symbiont's official SDK consumers.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, Response},
    response::IntoResponse,
    routing::{get, post},
};
use infer_runtime_client::{CAPABILITY_CONTRACT_HEADER, CONSUMER_CORE, CONSUMER_CORE_HEADER};
use serde_json::{Value, json};

const RESPONSES_SCHEMA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/infer-runtime/infer.responses.openapi.json"
));
const AUDIO_SCHEMA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/infer-runtime/infer.audio.transcription.openapi.json"
));

#[derive(Clone, Debug)]
pub(crate) enum CapturedBody {
    Json(Value),
    Multipart(String),
    None,
}

#[derive(Clone, Debug)]
pub(crate) struct Observation {
    pub(crate) path: &'static str,
    pub(crate) core_contract: Option<String>,
    pub(crate) capability_contract: Option<String>,
    pub(crate) authorized: bool,
    pub(crate) body: CapturedBody,
}

#[derive(Clone, Default)]
struct FakeState {
    observations: Arc<Mutex<Vec<Observation>>>,
}

pub(crate) struct FakeSdkRuntime {
    pub(crate) endpoint: String,
    observations: Arc<Mutex<Vec<Observation>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeSdkRuntime {
    pub(crate) fn observations(&self) -> Vec<Observation> {
        self.observations.lock().unwrap().clone()
    }
}

impl Drop for FakeSdkRuntime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn spawn() -> FakeSdkRuntime {
    let state = FakeState::default();
    let app = Router::new()
        .route("/infer/v1/capabilities", get(capabilities))
        .route(
            "/infer/v1/capability-schemas/infer.responses/20260812.1/openapi.json",
            get(responses_schema),
        )
        .route(
            "/infer/v1/capability-schemas/infer.audio.transcription/20260811.1/openapi.json",
            get(audio_schema),
        )
        .route("/v1/responses", post(response))
        .route("/infer/v1/jobs/{job_id}", get(job))
        .route("/v1/audio/transcriptions", post(transcription))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FakeSdkRuntime {
        endpoint,
        observations: state.observations,
        task,
    }
}

async fn capabilities(State(state): State<FakeState>, headers: HeaderMap) -> Json<Value> {
    record(
        &state,
        "/infer/v1/capabilities",
        &headers,
        CapturedBody::None,
    );
    Json(json!({
        "schema": "infer-runtime.capability-catalog",
        "schema_version": "20260813.1",
        "core_contract": CONSUMER_CORE,
        "capabilities": [
            {
                "id": "infer.responses",
                "schema_version": "20260812.1",
                "stability": "stable",
                "schema": {
                    "format": "openapi-3.1",
                    "url": "/infer/v1/capability-schemas/infer.responses/20260812.1/openapi.json",
                    "sha256": "abfb3b4b9a3c5d3831d56bb877ecfdd43d62b4442ba101a5ef071ec2740adbd5"
                },
                "routes": [{"method": "POST", "path": "/v1/responses", "execution_modes": ["unary"]}]
            },
            {
                "id": "infer.audio.transcription",
                "schema_version": "20260811.1",
                "stability": "stable",
                "schema": {
                    "format": "openapi-3.1",
                    "url": "/infer/v1/capability-schemas/infer.audio.transcription/20260811.1/openapi.json",
                    "sha256": "53ee5993abbaa3ccc04a5b5f77f3457fbd2f29cccda0b33b0959b6b900c25e59"
                },
                "routes": [{"method": "POST", "path": "/v1/audio/transcriptions", "execution_modes": ["unary"]}]
            }
        ]
    }))
}

async fn responses_schema(State(state): State<FakeState>, headers: HeaderMap) -> impl IntoResponse {
    record(
        &state,
        "/infer/v1/capability-schemas/infer.responses/20260812.1/openapi.json",
        &headers,
        CapturedBody::None,
    );
    schema_response(RESPONSES_SCHEMA)
}

async fn audio_schema(State(state): State<FakeState>, headers: HeaderMap) -> impl IntoResponse {
    record(
        &state,
        "/infer/v1/capability-schemas/infer.audio.transcription/20260811.1/openapi.json",
        &headers,
        CapturedBody::None,
    );
    schema_response(AUDIO_SCHEMA)
}

fn schema_response(bytes: &'static [u8]) -> Response<Body> {
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .unwrap()
}

async fn response(
    State(state): State<FakeState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    record(
        &state,
        "/v1/responses",
        &headers,
        CapturedBody::Json(request),
    );
    Json(json!({
        "id": "response_fixture",
        "object": "response",
        "created_at": 1,
        "model": "language.respond",
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "fixture response"}]
        }],
        "usage": {
            "input_tokens": 4,
            "output_tokens": 2,
            "total_tokens": 6
        }
    }))
}

async fn job(
    State(state): State<FakeState>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
) -> Json<Value> {
    record(
        &state,
        "/infer/v1/jobs/{job_id}",
        &headers,
        CapturedBody::None,
    );
    Json(json!({
        "id": job_id,
        "app_id": "symbiont-d",
        "intent": "language.respond",
        "consumer_core_contract": CONSUMER_CORE,
        "capability_contract": "infer.responses@20260812.1",
        "provider": "fixture-provider",
        "deployment": "fixture-deployment",
        "model_profile": "fixture-profile",
        "model_build": "fixture-build",
        "physical_model": "fixture-model",
        "placement": "local",
        "capability_level": "advanced",
        "evaluation_status": "evaluated",
        "resource_class": "general",
        "state": "completed",
        "policy": "balanced",
        "priority": "background",
        "constraints": {},
        "routing": {
            "capability_floor": "advanced",
            "named_route": null,
            "candidates": []
        },
        "attempts": [],
        "error": null
    }))
}

async fn transcription(
    State(state): State<FakeState>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<Value> {
    record(
        &state,
        "/v1/audio/transcriptions",
        &headers,
        CapturedBody::Multipart(String::from_utf8_lossy(&body).into_owned()),
    );
    Json(json!({
        "id": "transcription_fixture",
        "model": "audio.transcribe",
        "text": "fixture transcript",
        "language": "zh",
        "segments": [],
        "usage": {}
    }))
}

fn record(state: &FakeState, path: &'static str, headers: &HeaderMap, body: CapturedBody) {
    state.observations.lock().unwrap().push(Observation {
        path,
        core_contract: header(headers, CONSUMER_CORE_HEADER),
        capability_contract: header(headers, CAPABILITY_CONTRACT_HEADER),
        authorized: headers.contains_key("authorization"),
        body,
    });
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
