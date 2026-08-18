//! Compatibility surface for the PCP Runtime worker protocol.
//!
//! Runtime v0.8 currently disables semantic maintenance. Symbiont therefore
//! does not construct prompts or model decisions for these requests.

use anyhow::Result;
use pcp_runtime::{MaintenanceWorkerRequest, MaintenanceWorkerResponse};

pub(super) const RUNTIME_INSTRUCTIONS: &str =
    "PCP v0.8 semantic maintenance is disabled for this tenant.";

pub(super) fn runtime_prompt(_request: &MaintenanceWorkerRequest) -> Result<String> {
    Ok("Return the protocol decision `defer`.".to_owned())
}

pub(super) fn validate_response(
    _request: &MaintenanceWorkerRequest,
    response: &MaintenanceWorkerResponse,
) -> Result<()> {
    anyhow::ensure!(
        matches!(response, MaintenanceWorkerResponse::Defer),
        "PCP v0.8 Symbiont worker only supports defer while semantic maintenance is disabled"
    );
    Ok(())
}
