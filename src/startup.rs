use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use tokio::sync::{RwLock, watch};
use tower::ServiceExt;

use crate::web::{self, AppState};

/// Owns the local HTTP surface while Codex is reconnecting.  The complete app
/// is swapped in only after its dependency graph is ready, so application
/// workers never observe a half-initialized Codex client.
#[derive(Clone)]
pub struct StartupShell {
    app: Arc<RwLock<Option<AppState>>>,
    status: Arc<RwLock<String>>,
    retry: watch::Sender<u64>,
}

impl StartupShell {
    pub fn new() -> Self {
        Self {
            app: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new("正在连接 Codex…".to_owned())),
            retry: watch::channel(0).0,
        }
    }

    pub async fn set_status(&self, status: impl Into<String>) {
        *self.status.write().await = status.into();
    }

    pub async fn set_ready(&self, app: AppState) {
        *self.app.write().await = Some(app);
    }

    pub fn retry_subscriber(&self) -> watch::Receiver<u64> {
        self.retry.subscribe()
    }

    fn request_retry(&self) {
        let revision = self.retry.borrow().wrapping_add(1);
        self.retry.send_replace(revision);
    }

    pub fn router(self) -> Router {
        Router::new().fallback(any(dispatch)).with_state(self)
    }
}

async fn dispatch(State(shell): State<StartupShell>, request: Request) -> Response {
    if let Some(app) = shell.app.read().await.clone() {
        return web::router(app)
            .oneshot(request)
            .await
            .unwrap_or_else(|never: Infallible| match never {});
    }
    let path = request.uri().path();
    if request.method() == axum::http::Method::POST && path == "/api/retry" {
        shell.request_retry();
        return (
            StatusCode::ACCEPTED,
            axum::Json(serde_json::json!({
                "accepted": true,
                "status": "retrying",
            })),
        )
            .into_response();
    }
    if path.starts_with("/api/") {
        let status = shell.status.read().await.clone();
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("Retry-After", "5")],
            axum::Json(serde_json::json!({
                "error": "codex_reconnecting",
                "message": status,
                "retryable": true,
            })),
        )
            .into_response();
    }
    let status = shell.status.read().await.clone();
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("content-type", "text/html; charset=utf-8"), ("retry-after", "5")],
        format!(
            "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>symbiont-d</title><style>body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#faf9f7;color:#272625;font:16px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif}}main{{max-width:430px;padding:36px;text-align:center}}h1{{margin:0 0 12px;font-size:24px}}p{{color:#6d6a67;line-height:1.6}}button{{margin-top:12px;padding:9px 14px;border:1px solid #bbb7b2;border-radius:999px;background:white;font:inherit;cursor:pointer}}</style><main><h1>symbiont-d 正在恢复连接</h1><p>{}</p><button id=\"retry\">立即重试</button></main><script>const b=document.querySelector('#retry');b.onclick=async()=>{{b.disabled=true;b.textContent='正在重新发起连接…';try{{const r=await fetch('/api/retry',{{method:'POST'}});if(!r.ok)throw new Error()}}catch{{b.disabled=false;b.textContent='重试失败，请再试一次';return}}setTimeout(()=>location.reload(),350)}};setTimeout(()=>location.reload(),5000)</script></html>",
            html_escape(&status)
        ),
    )
        .into_response()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub async fn serve(listener: tokio::net::TcpListener, shell: StartupShell) -> anyhow::Result<()> {
    axum::serve(listener, shell.router())
        .await
        .map_err(anyhow::Error::from)
}

pub async fn bind(bind: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(bind)
        .await
        .map_err(anyhow::Error::from)
}
