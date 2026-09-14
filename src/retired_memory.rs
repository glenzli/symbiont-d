//! Tombstones for the retired tenant-side PCP maintenance UI/API.
//!
//! No application state, store, worker or model dependency. Existing
//! reconciliation.json and usage/trace history are left untouched; stale
//! clients must never interpret retirement as an empty scan.

use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/api/reconciliation", get(retired))
        .route("/api/reconciliation/preview", post(retired))
        .route("/api/reconciliation/apply/{run_id}", post(retired))
        .route("/api/internal/pcp-maintenance/evaluate", post(retired))
        .route("/api/signal-retention", post(retired_signal_retention))
}

async fn retired_signal_retention() -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "code": "signal_retention_retired",
            "accepted": false,
            "error": "旧版外部输入自动清理已退役。未回复输入超过 24 小时仅从聊天隐藏，原文与异议保留在按日期查看的历史中。",
            "historyPreserved": true
        })),
    )
}

async fn retired() -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "code": "memory_maintenance_retired",
            "accepted": false,
            "error": "旧版记忆整理已退役。PCP 库维护由 PCP Runtime 负责；后台对话整理与自主记忆写入不受影响。",
            "historyPreserved": true
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn stale_retention_clients_cannot_change_or_delete_history() {
        let response = routes::<()>()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/signal-retention")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"retentionDays":3}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "signal_retention_retired");
        assert_eq!(body["accepted"], false);
        assert_eq!(body["historyPreserved"], true);
    }

    #[tokio::test]
    async fn stale_clients_cannot_start_or_apply_memory_maintenance() {
        for (method, path) in [
            ("GET", "/api/reconciliation"),
            ("POST", "/api/reconciliation/preview"),
            (
                "POST",
                "/api/reconciliation/apply/old-run?overrideTokenLimit=true",
            ),
            ("POST", "/api/internal/pcp-maintenance/evaluate"),
        ] {
            // No initialized application, credential, database or model needed.
            let response = routes::<()>()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::GONE, "{path}");
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["accepted"], false);
            assert_eq!(body["code"], "memory_maintenance_retired");
            assert_eq!(body["historyPreserved"], true);
        }
    }
}
