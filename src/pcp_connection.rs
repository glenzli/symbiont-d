mod enrollment;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, RwLock, Weak},
    time::Duration,
};

use anyhow::{Context, Result};
use pcp_client::{
    DurablePageInventoryItem, EmbeddedPcpClient, HealthSnapshot, PcpApi, TombstoneCascadeResult,
};
use pcp_core::{
    AccessAuditEvent, AccessSession, Actor, AssessPageValidityRequest, Capabilities,
    CollectRevisionRetentionRequest, ConsolidatePagesRequest, CreateScopeRequest, LinkPagesRequest,
    PlanRevisionRetentionRequest, PutRevisionRetentionLeaseRequest, ReadPage, ReadPagesRequest,
    Relation, RevisePageRequest, RevisionCollectionResult, RevisionRetentionLease,
    RevisionRetentionPlan, Scope, SearchPagesRequest, SearchResult, WritePageRequest, WriteResult,
    WriteSummaryRequest, WriteSummaryResult, WriteValidityResult,
};
use pcp_rpc::RemotePcpClient;
use pcp_sqlite::SqlitePcpStore;
use pcp_store::PcpStore;
use tokio::{sync::Mutex, time::Instant};
use tracing::{info, warn};

use self::enrollment::{ActiveEnrollment, EnrollmentManager, EnrollmentProbe, HOST_PRINCIPAL_ID};
use crate::continuity::ContinuityHost;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ActiveClient {
    api: Arc<dyn PcpApi>,
    generation: Option<String>,
    enrolled: bool,
}

/// Keeps the transport replaceable while presenting the stable Store identity,
/// principal, and grant set expected by the rest of Symbiont. Enrollment RPC
/// session IDs are generation-specific, so recovery verifies but does not
/// expose the replacement ID through this stable facade.
struct ManagedPcpClient {
    owner_id: String,
    access: AccessSession,
    active: RwLock<ActiveClient>,
    enrollment: Option<Arc<EnrollmentManager>>,
    static_socket: Option<PathBuf>,
    recovery: Mutex<()>,
    connect_timeout: Duration,
}

pub async fn open(workspace: &Path) -> Result<Arc<dyn PcpApi>> {
    let connect_timeout = runtime_connect_timeout()?;
    let static_socket = env::var_os("SYMBIONT_PCP_RUNTIME_SOCKET").map(PathBuf::from);
    let enrollment = EnrollmentManager::open(workspace).await?.map(Arc::new);

    if let Some(socket_path) = static_socket.as_ref() {
        if let Some(manager) = enrollment.as_ref()
            && manager.has_selected_instance().await
        {
            match manager.probe(None).await {
                Ok(EnrollmentProbe::Active(active)) => {
                    return Ok(managed_client(
                        active_client(active),
                        Some(Arc::clone(manager)),
                        static_socket,
                        connect_timeout,
                    ));
                }
                Ok(EnrollmentProbe::Pending) => {
                    info!(
                        "PCP Runtime enrollment is pending approval; retaining the migration fallback"
                    );
                }
                Ok(EnrollmentProbe::Rejected) => {
                    warn!("PCP Runtime enrollment was rejected; retaining the migration fallback");
                }
                Ok(EnrollmentProbe::Unavailable) => {}
                Err(error) => {
                    warn!(%error, "PCP Runtime discovery or enrollment is not ready; retaining the migration fallback");
                }
            }
        }
        let fallback = connect_configured(socket_path, connect_timeout).await?;
        if let Some(manager) = enrollment.as_ref() {
            match manager.probe(Some(fallback.owner_id())).await {
                Ok(EnrollmentProbe::Active(active)) => {
                    return Ok(managed_client(
                        active_client(active),
                        Some(Arc::clone(manager)),
                        static_socket,
                        connect_timeout,
                    ));
                }
                Ok(
                    EnrollmentProbe::Pending
                    | EnrollmentProbe::Rejected
                    | EnrollmentProbe::Unavailable,
                ) => {}
                Err(error) => {
                    warn!(%error, "could not promote the configured PCP fallback to enrollment yet");
                }
            }
        }
        return Ok(managed_client(
            ActiveClient {
                api: Arc::new(fallback),
                generation: None,
                enrolled: false,
            },
            enrollment,
            static_socket,
            connect_timeout,
        ));
    }

    if let Some(manager) = enrollment.as_ref() {
        match manager.probe(None).await? {
            EnrollmentProbe::Active(active) => {
                return Ok(managed_client(
                    active_client(active),
                    Some(Arc::clone(manager)),
                    None,
                    connect_timeout,
                ));
            }
            EnrollmentProbe::Pending => {
                anyhow::bail!("PCP Runtime enrollment is pending approval in PCP Console")
            }
            EnrollmentProbe::Rejected => {
                anyhow::bail!("PCP Runtime enrollment was rejected in PCP Console")
            }
            EnrollmentProbe::Unavailable => {}
        }
    }

    open_embedded(workspace).await
}

fn active_client(active: ActiveEnrollment) -> ActiveClient {
    ActiveClient {
        api: Arc::new(active.client),
        generation: Some(active.generation),
        enrolled: true,
    }
}

fn managed_client(
    initial: ActiveClient,
    enrollment: Option<Arc<EnrollmentManager>>,
    static_socket: Option<PathBuf>,
    connect_timeout: Duration,
) -> Arc<dyn PcpApi> {
    let owner_id = initial.api.owner_id().to_owned();
    let access = initial.api.access().clone();
    let client = Arc::new(ManagedPcpClient {
        owner_id,
        access,
        active: RwLock::new(initial),
        enrollment,
        static_socket,
        recovery: Mutex::new(()),
        connect_timeout,
    });
    ManagedPcpClient::spawn_monitor(&client);
    client
}

impl ManagedPcpClient {
    fn current(&self) -> ActiveClient {
        self.active
            .read()
            .expect("PCP connection lock poisoned")
            .clone()
    }

    fn install(&self, next: ActiveClient) -> Result<()> {
        anyhow::ensure!(
            next.api.owner_id() == self.owner_id,
            "recovered PCP Store identity does not match the active Store"
        );
        anyhow::ensure!(
            equivalent_access(&self.access, next.api.access()),
            "recovered PCP session changed the approved principal or grants"
        );
        *self.active.write().expect("PCP connection lock poisoned") = next;
        Ok(())
    }

    async fn refresh_enrollment(&self, force_reopen: bool) -> Result<bool> {
        let Some(manager) = self.enrollment.as_ref() else {
            return Ok(false);
        };
        if !force_reopen {
            let current = self.current();
            let Some(discovered_generation) =
                manager.discovered_generation(Some(&self.owner_id)).await?
            else {
                return Ok(false);
            };
            if current.enrolled
                && current.generation.as_deref() == Some(discovered_generation.as_str())
            {
                return Ok(false);
            }
        }
        match manager.probe(Some(&self.owner_id)).await? {
            EnrollmentProbe::Active(active) => {
                let current = self.current();
                if !force_reopen
                    && current.enrolled
                    && current.generation.as_deref() == Some(&active.generation)
                {
                    return Ok(false);
                }
                let generation = active.generation.clone();
                self.install(active_client(active))?;
                info!(%generation, "opened the approved PCP session for the current Runtime generation");
                Ok(true)
            }
            EnrollmentProbe::Pending | EnrollmentProbe::Rejected | EnrollmentProbe::Unavailable => {
                Ok(false)
            }
        }
    }

    async fn recover_after_transport_failure(&self, failed: &ActiveClient) -> Result<()> {
        let _recovery = self.recovery.lock().await;
        let current = self.current();
        if !Arc::ptr_eq(&current.api, &failed.api) {
            return Ok(());
        }
        if self.refresh_enrollment(true).await.unwrap_or(false) {
            return Ok(());
        }
        if let Some(socket_path) = self.static_socket.as_ref() {
            let fallback = connect_configured(socket_path, self.connect_timeout).await?;
            self.install(ActiveClient {
                api: Arc::new(fallback),
                generation: None,
                enrolled: false,
            })?;
            return Ok(());
        }
        anyhow::bail!("no approved PCP session or configured migration fallback is available")
    }

    fn spawn_monitor(client: &Arc<Self>) {
        if client.enrollment.is_none() {
            return;
        }
        let client = Arc::downgrade(client);
        tokio::spawn(async move {
            monitor_enrollment(client).await;
        });
    }
}

async fn monitor_enrollment(client: Weak<ManagedPcpClient>) {
    let mut interval = tokio::time::interval(DISCOVERY_REFRESH_INTERVAL);
    interval.tick().await;
    loop {
        interval.tick().await;
        let Some(client) = client.upgrade() else {
            return;
        };
        if let Err(error) = client.refresh_enrollment(false).await {
            warn!(%error, "could not refresh the discovered PCP Runtime session");
        }
    }
}

fn equivalent_access(stable: &AccessSession, current: &AccessSession) -> bool {
    stable.principal.principal_id == current.principal.principal_id
        && stable.principal.principal_type == current.principal.principal_type
        && grant_set(stable) == grant_set(current)
}

fn grant_set(access: &AccessSession) -> BTreeMap<String, BTreeSet<pcp_core::AccessPermission>> {
    let mut grants = BTreeMap::<_, BTreeSet<_>>::new();
    for grant in &access.grants {
        grants
            .entry(grant.namespace.clone())
            .or_default()
            .extend(grant.permissions.iter().copied());
    }
    grants
}

fn transport_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<std::io::Error>())
        || error.chain().any(|cause| {
            let message = cause.to_string();
            message.contains("timed out")
                || message.contains("closed before responding")
                || message.contains("closed without a response")
        })
}

async fn connect_configured(path: &Path, timeout: Duration) -> Result<RemotePcpClient> {
    let started = Instant::now();
    loop {
        match RemotePcpClient::connect_expected(path, HOST_PRINCIPAL_ID).await {
            Ok(client) => return Ok(client),
            Err(error) if started.elapsed() >= timeout => {
                return Err(error).with_context(|| {
                    format!(
                        "connect configured PCP runtime at {} within {} ms; embedded fallback is disabled",
                        path.display(),
                        timeout.as_millis()
                    )
                });
            }
            Err(_) => tokio::time::sleep(CONNECT_RETRY_INTERVAL).await,
        }
    }
}

async fn open_embedded(workspace: &Path) -> Result<Arc<dyn PcpApi>> {
    let path = env::var_os("SYMBIONT_PCP_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("data/context.sqlite3"));
    let store = Arc::new(SqlitePcpStore::open(path).await?);
    let access = ContinuityHost::access_session(store.owner_id());
    let store: Arc<dyn PcpStore> = store;
    Ok(EmbeddedPcpClient::shared(store, access))
}

fn runtime_connect_timeout() -> Result<Duration> {
    let Some(value) = env::var_os("SYMBIONT_PCP_CONNECT_TIMEOUT_MS") else {
        return Ok(DEFAULT_CONNECT_TIMEOUT);
    };
    let value = value
        .to_str()
        .context("SYMBIONT_PCP_CONNECT_TIMEOUT_MS must be valid UTF-8")?;
    let milliseconds = value
        .parse::<u64>()
        .context("SYMBIONT_PCP_CONNECT_TIMEOUT_MS must be an integer")?;
    Ok(Duration::from_millis(milliseconds))
}

type PcpFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

macro_rules! retrying_read {
    ($name:ident ( $( $argument:ident : $argument_type:ty ),* $(,)? ) -> $output:ty) => {
        fn $name<'life0, 'async_trait>(
            &'life0 self,
            $( $argument: $argument_type ),*
        ) -> PcpFuture<'async_trait, $output>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let first = self.current();
                match first.api.$name($( $argument.clone() ),*).await {
                    Ok(value) => Ok(value),
                    Err(error) if transport_failure(&error) => {
                        self.recover_after_transport_failure(&first).await?;
                        self.current().api.$name($( $argument ),*).await
                    }
                    Err(error) => Err(error),
                }
            })
        }
    };
}

macro_rules! recovering_write {
    ($name:ident ( $( $argument:ident : $argument_type:ty ),* $(,)? ) -> $output:ty) => {
        fn $name<'life0, 'async_trait>(
            &'life0 self,
            $( $argument: $argument_type ),*
        ) -> PcpFuture<'async_trait, $output>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let first = self.current();
                match first.api.$name($( $argument ),*).await {
                    Ok(value) => Ok(value),
                    Err(error) if transport_failure(&error) => {
                        let _ = self.recover_after_transport_failure(&first).await;
                        Err(error).context(concat!(
                            "PCP transport recovered after an ambiguous ",
                            stringify!($name),
                            "; the mutation was not retried"
                        ))
                    }
                    Err(error) => Err(error),
                }
            })
        }
    };
}

impl PcpApi for ManagedPcpClient {
    fn owner_id(&self) -> &str {
        &self.owner_id
    }

    fn capabilities(&self) -> Capabilities {
        self.current().api.capabilities()
    }

    fn access(&self) -> &AccessSession {
        &self.access
    }

    retrying_read!(integrity_check() -> String);
    recovering_write!(create_scope(request: CreateScopeRequest) -> ());
    retrying_read!(list_scopes(
        requested_scopes: Vec<String>,
        query: Option<String>,
        limit: u32,
        cursor: Option<String>,
    ) -> (Vec<Scope>, Option<String>));
    retrying_read!(search_pages(request: SearchPagesRequest) -> SearchResult);
    retrying_read!(browse_index(
        scopes: Vec<String>,
        excluded_page_kinds: Vec<String>,
        limit: u32,
        cursor: Option<String>,
        max_chars: u32,
    ) -> SearchResult);
    retrying_read!(read_pages(request: ReadPagesRequest) -> Vec<ReadPage>);
    retrying_read!(current_revision_id(page_id: String) -> String);
    retrying_read!(page_count(requested_scopes: Vec<String>) -> u64);
    retrying_read!(content_char_count(requested_scopes: Vec<String>) -> usize);
    retrying_read!(plan_revision_retention(
        request: PlanRevisionRetentionRequest,
    ) -> RevisionRetentionPlan);
    recovering_write!(collect_revision_retention(
        request: CollectRevisionRetentionRequest,
    ) -> RevisionCollectionResult);
    recovering_write!(put_revision_retention_lease(
        request: PutRevisionRetentionLeaseRequest,
    ) -> RevisionRetentionLease);
    retrying_read!(active_revision_retention_leases(
        requested_scopes: Vec<String>,
        limit: u32,
    ) -> Vec<RevisionRetentionLease>);
    recovering_write!(write_page(request: WritePageRequest) -> WriteResult);
    recovering_write!(revise_page(request: RevisePageRequest) -> WriteResult);
    recovering_write!(consolidate_pages(request: ConsolidatePagesRequest) -> WriteResult);
    recovering_write!(link_pages(request: LinkPagesRequest) -> Relation);
    recovering_write!(write_summary(request: WriteSummaryRequest) -> WriteSummaryResult);
    retrying_read!(next_summary_candidate(
        minimum_chars: usize,
        excluded_page_kinds: Vec<String>,
    ) -> Option<String>);
    recovering_write!(mark_summary_assessed(
        target_revision_id: String,
        outcome: String,
        tool_or_model: Option<String>,
    ) -> ());
    recovering_write!(assess_page_validity(
        request: AssessPageValidityRequest,
    ) -> WriteValidityResult);
    recovering_write!(tombstone_derivation_cascade(
        root_revision_id: String,
        actor: Actor,
    ) -> TombstoneCascadeResult);
    retrying_read!(durable_page_inventory(
        excluded_page_kinds: Vec<String>,
    ) -> Vec<DurablePageInventoryItem>);
    retrying_read!(access_log(
        limit: u32,
        cursor: Option<String>,
    ) -> (Vec<AccessAuditEvent>, Option<String>));
    retrying_read!(health_snapshot(
        requested_scopes: Vec<String>,
        window_hours: u32,
    ) -> HealthSnapshot);
}

#[cfg(test)]
mod tests {
    use pcp_core::{AccessPermission, AccessPrincipal, AccessPrincipalType, ScopeGrant};

    use super::*;

    #[test]
    fn generation_specific_metadata_and_grant_order_do_not_change_the_approved_policy() {
        let first_principal = AccessPrincipal {
            principal_id: HOST_PRINCIPAL_ID.to_owned(),
            principal_type: AccessPrincipalType::Host,
            display_name: Some("Symbiont".to_owned()),
        };
        let reopened_principal = AccessPrincipal {
            display_name: Some("Symbiont host".to_owned()),
            ..first_principal.clone()
        };
        let first = AccessSession::new(
            first_principal,
            "enrolled:reg:proc-1",
            vec![
                ScopeGrant {
                    namespace: "project:symbiont-d".to_owned(),
                    permissions: vec![AccessPermission::ReadDetail],
                },
                ScopeGrant {
                    namespace: "project:symbiont-d".to_owned(),
                    permissions: vec![AccessPermission::Write],
                },
            ],
        );
        let reopened = AccessSession::new(
            reopened_principal,
            "enrolled:reg:proc-2",
            vec![ScopeGrant {
                namespace: "project:symbiont-d".to_owned(),
                permissions: vec![AccessPermission::Write, AccessPermission::ReadDetail],
            }],
        );
        assert!(equivalent_access(&first, &reopened));
    }

    #[test]
    fn changed_grants_are_not_accepted_during_recovery() {
        let principal = AccessPrincipal {
            principal_id: HOST_PRINCIPAL_ID.to_owned(),
            principal_type: AccessPrincipalType::Host,
            display_name: Some("Symbiont".to_owned()),
        };
        let first = AccessSession::new(
            principal.clone(),
            "first",
            vec![ScopeGrant {
                namespace: "project:symbiont-d".to_owned(),
                permissions: vec![AccessPermission::ReadDetail],
            }],
        );
        let changed = AccessSession::new(
            principal,
            "second",
            vec![ScopeGrant {
                namespace: "project:symbiont-d".to_owned(),
                permissions: vec![AccessPermission::Write],
            }],
        );
        assert!(!equivalent_access(&first, &changed));
    }

    #[test]
    fn application_errors_do_not_trigger_transport_recovery() {
        let error = anyhow::anyhow!("PCP runtime: relation would introduce a cycle");
        assert!(!transport_failure(&error));
        let transport = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "offline",
        ));
        assert!(transport_failure(&transport));
    }
}
