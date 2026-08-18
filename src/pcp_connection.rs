mod enrollment;

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, RwLock, Weak},
    time::Duration,
};

use anyhow::{Context, Result};
use pcp_client::PcpTenantApi;
use pcp_core::{
    AccessSession, Capabilities, IngestPageRequest, ReadPage, ReadPagesRequest, Scope,
    SearchPagesRequest, SearchResult, WriteResult,
};
use tokio::sync::Mutex;
use tracing::{info, warn};

use self::enrollment::{ActiveEnrollment, EnrollmentManager, EnrollmentProbe};
const DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ActiveClient {
    api: Arc<dyn PcpTenantApi>,
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
    recovery: Mutex<()>,
}

pub async fn open(workspace: &Path) -> Result<Arc<dyn PcpTenantApi>> {
    let enrollment = EnrollmentManager::open(workspace).await?.map(Arc::new);
    let manager = enrollment.ok_or_else(|| {
        anyhow::anyhow!(
            "PCP Runtime discovery is unavailable on this platform; start PCP Console and approve Symbiont enrollment"
        )
    })?;
    match manager.probe(None).await? {
        EnrollmentProbe::Active(active) => Ok(managed_client(active_client(active), Some(manager))),
        EnrollmentProbe::Pending => {
            anyhow::bail!("PCP Runtime enrollment is pending approval in PCP Console")
        }
        EnrollmentProbe::Rejected => {
            anyhow::bail!("PCP Runtime enrollment was rejected in PCP Console")
        }
        EnrollmentProbe::Unavailable => anyhow::bail!(
            "no PCP Runtime is discoverable; start PCP Console and approve Symbiont enrollment"
        ),
    }
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
) -> Arc<dyn PcpTenantApi> {
    let owner_id = initial.api.identity_id().to_owned();
    let access = initial.api.access().clone();
    let client = Arc::new(ManagedPcpClient {
        owner_id,
        access,
        active: RwLock::new(initial),
        enrollment,
        recovery: Mutex::new(()),
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
            next.api.identity_id() == self.owner_id,
            "recovered PCP Identity does not match the active Identity"
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
        anyhow::bail!("no approved PCP session is available through discovery")
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

#[async_trait::async_trait]
impl PcpTenantApi for ManagedPcpClient {
    fn identity_id(&self) -> &str {
        &self.owner_id
    }

    fn capabilities(&self) -> Capabilities {
        self.current().api.capabilities()
    }

    fn access(&self) -> &AccessSession {
        &self.access
    }

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
    recovering_write!(ingest_page(request: IngestPageRequest) -> WriteResult);
}

#[cfg(test)]
mod tests {
    use pcp_core::{AccessPermission, AccessPrincipal, AccessPrincipalType, ScopeGrant};

    use super::*;

    #[test]
    fn generation_specific_metadata_and_grant_order_do_not_change_the_approved_policy() {
        let first_principal = AccessPrincipal {
            principal_id: enrollment::HOST_PRINCIPAL_ID.to_owned(),
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
            principal_id: enrollment::HOST_PRINCIPAL_ID.to_owned(),
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
