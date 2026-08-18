use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::{Mutex, oneshot, watch},
    time::timeout,
};

const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionResolutionSource {
    User,
    SessionGrant,
    BackgroundPolicy,
    Timeout,
    BrokerClosed,
}

#[derive(Clone, Debug)]
pub struct PermissionResolution {
    pub decision: PermissionDecision,
    pub source: PermissionResolutionSource,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequestView {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub origin: String,
    pub title: String,
    pub reason: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub protocol: Option<String>,
    pub details: Value,
    pub requested_at: String,
    pub expires_at: String,
    pub allow_accept: bool,
    pub allow_session: bool,
    pub allow_cancel: bool,
}

pub struct PermissionRequestDraft {
    pub kind: String,
    pub source: String,
    pub origin: String,
    pub title: String,
    pub reason: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub protocol: Option<String>,
    pub details: Value,
    pub allow_accept: bool,
    pub allow_session: bool,
    pub allow_cancel: bool,
    pub session_key: Option<String>,
    pub timeout: Option<Duration>,
}

struct PendingPermission {
    view: PermissionRequestView,
    session_key: Option<String>,
    sender: oneshot::Sender<PermissionDecision>,
}

#[derive(Default)]
struct BrokerState {
    pending: HashMap<String, PendingPermission>,
    session_grants: HashSet<String>,
}

pub struct PermissionBroker {
    state: Mutex<BrokerState>,
    sequence: AtomicU64,
    changes: watch::Sender<u64>,
}

impl Default for PermissionBroker {
    fn default() -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            state: Mutex::new(BrokerState::default()),
            sequence: AtomicU64::new(0),
            changes,
        }
    }
}

impl PermissionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> Vec<PermissionRequestView> {
        let mut requests = self
            .state
            .lock()
            .await
            .pending
            .values()
            .map(|pending| pending.view.clone())
            .collect::<Vec<_>>();
        requests.sort_by(|left, right| left.requested_at.cmp(&right.requested_at));
        requests
    }

    pub async fn request(&self, draft: PermissionRequestDraft) -> PermissionResolution {
        if let Some(session_key) = draft.session_key.as_ref()
            && self.state.lock().await.session_grants.contains(session_key)
        {
            return PermissionResolution {
                decision: PermissionDecision::AcceptForSession,
                source: PermissionResolutionSource::SessionGrant,
            };
        }
        if !origin_can_prompt(&draft.origin) {
            return PermissionResolution {
                decision: PermissionDecision::Decline,
                source: PermissionResolutionSource::BackgroundPolicy,
            };
        }

        let requested_at = Utc::now();
        let approval_timeout = draft.timeout.unwrap_or(DEFAULT_APPROVAL_TIMEOUT);
        let expires_at = requested_at
            + chrono::Duration::from_std(approval_timeout)
                .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let id = format!(
            "permission_{}_{}",
            requested_at.timestamp_millis(),
            self.sequence.fetch_add(1, Ordering::Relaxed)
        );
        let view = PermissionRequestView {
            id: id.clone(),
            kind: draft.kind,
            source: draft.source,
            origin: draft.origin,
            title: draft.title,
            reason: draft.reason,
            command: draft.command,
            cwd: draft.cwd,
            host: draft.host,
            protocol: draft.protocol,
            details: draft.details,
            requested_at: requested_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            allow_accept: draft.allow_accept,
            allow_session: draft.allow_session,
            allow_cancel: draft.allow_cancel,
        };
        let (sender, receiver) = oneshot::channel();
        self.state.lock().await.pending.insert(
            id.clone(),
            PendingPermission {
                view,
                session_key: draft.session_key,
                sender,
            },
        );
        self.notify_changed();

        match timeout(approval_timeout, receiver).await {
            Ok(Ok(decision)) => PermissionResolution {
                decision,
                source: PermissionResolutionSource::User,
            },
            Ok(Err(_)) => {
                self.state.lock().await.pending.remove(&id);
                self.notify_changed();
                PermissionResolution {
                    decision: PermissionDecision::Decline,
                    source: PermissionResolutionSource::BrokerClosed,
                }
            }
            Err(_) => {
                self.state.lock().await.pending.remove(&id);
                self.notify_changed();
                PermissionResolution {
                    decision: PermissionDecision::Decline,
                    source: PermissionResolutionSource::Timeout,
                }
            }
        }
    }

    pub async fn resolve(
        &self,
        id: &str,
        decision: PermissionDecision,
    ) -> Result<PermissionRequestView> {
        let mut state = self.state.lock().await;
        let view = state
            .pending
            .get(id)
            .map(|pending| pending.view.clone())
            .with_context(|| format!("permission request is no longer pending: {id}"))?;
        if matches!(
            decision,
            PermissionDecision::Accept | PermissionDecision::AcceptForSession
        ) && !view.allow_accept
        {
            anyhow::bail!("this request cannot be accepted without structured input");
        }
        if decision == PermissionDecision::AcceptForSession && !view.allow_session {
            anyhow::bail!("this permission request does not support session approval");
        }
        if decision == PermissionDecision::Cancel && !view.allow_cancel {
            anyhow::bail!("this permission request does not support cancellation");
        }
        let pending = state
            .pending
            .remove(id)
            .expect("pending permission was checked under the same lock");
        let session_key = pending.session_key.clone();
        pending
            .sender
            .send(decision)
            .map_err(|_| anyhow::anyhow!("the requesting Codex turn is no longer active"))?;
        if decision == PermissionDecision::AcceptForSession
            && let Some(session_key) = session_key
        {
            state.session_grants.insert(session_key);
        }
        drop(state);
        self.notify_changed();
        Ok(view)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn notify_changed(&self) {
        let next = self.changes.borrow().wrapping_add(1);
        self.changes.send_replace(next);
    }
}

fn origin_can_prompt(origin: &str) -> bool {
    origin == "interactive"
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use serde_json::json;

    use super::{
        PermissionBroker, PermissionDecision, PermissionRequestDraft, PermissionResolutionSource,
    };

    fn request(origin: &str, session_key: Option<&str>) -> PermissionRequestDraft {
        PermissionRequestDraft {
            kind: "networkAccess".to_owned(),
            source: "symbiont".to_owned(),
            origin: origin.to_owned(),
            title: "访问 example.com".to_owned(),
            reason: None,
            command: None,
            cwd: None,
            host: Some("example.com".to_owned()),
            protocol: Some("https".to_owned()),
            details: json!({}),
            allow_accept: true,
            allow_session: true,
            allow_cancel: false,
            session_key: session_key.map(str::to_owned),
            timeout: Some(Duration::from_secs(1)),
        }
    }

    #[tokio::test]
    async fn background_requests_are_declined_without_entering_pending_state() {
        let broker = PermissionBroker::new();
        let resolution = broker.request(request("autonomous", None)).await;
        assert_eq!(resolution.decision, PermissionDecision::Decline);
        assert_eq!(
            resolution.source,
            PermissionResolutionSource::BackgroundPolicy
        );
        assert!(broker.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn interactive_session_grant_is_reused() {
        let broker = Arc::new(PermissionBroker::new());
        let waiting = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move {
                broker
                    .request(request("interactive", Some("https:example.com")))
                    .await
            })
        };
        tokio::task::yield_now().await;
        let pending = broker.snapshot().await;
        assert_eq!(pending.len(), 1);
        broker
            .resolve(&pending[0].id, PermissionDecision::AcceptForSession)
            .await
            .unwrap();
        assert_eq!(
            waiting.await.unwrap().decision,
            PermissionDecision::AcceptForSession
        );
        let reused = broker
            .request(request("autonomous", Some("https:example.com")))
            .await;
        assert_eq!(reused.source, PermissionResolutionSource::SessionGrant);
    }

    #[tokio::test]
    async fn pending_permissions_notify_live_subscribers() {
        let broker = Arc::new(PermissionBroker::new());
        let mut updates = broker.subscribe();
        let waiting = {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move { broker.request(request("interactive", None)).await })
        };

        updates.changed().await.expect("pending notification");
        let pending = broker.snapshot().await;
        broker
            .resolve(&pending[0].id, PermissionDecision::Decline)
            .await
            .expect("resolve pending permission");
        updates.changed().await.expect("resolution notification");
        assert_eq!(
            waiting.await.expect("permission task").decision,
            PermissionDecision::Decline
        );
    }
}
