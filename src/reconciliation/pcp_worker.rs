use std::sync::Arc;

use anyhow::Result;
use pcp_runtime::{MaintenanceWorkerRequest, MaintenanceWorkerResponse};

use super::{ReconciliationDependencies, ReconciliationStore};

/// PCP Runtime owns maintenance policy.  Until it enables a v0.8 semantic
/// operation, Symbiont deliberately performs no inference and proposes no
/// write.  Keeping this worker protocol-compatible lets Runtime record a
/// bounded, explicit defer instead of accidentally reviving v0.7 behavior.
#[derive(Clone)]
pub(super) struct PcpMaintenanceWorker;

impl PcpMaintenanceWorker {
    pub(super) fn new(
        _store: Arc<ReconciliationStore>,
        _dependencies: ReconciliationDependencies,
    ) -> Self {
        Self
    }

    pub(super) async fn evaluate(
        &self,
        _request: MaintenanceWorkerRequest,
    ) -> Result<MaintenanceWorkerResponse> {
        Ok(MaintenanceWorkerResponse::Defer)
    }
}
