//! Retired compatibility shim for old operator configurations. Respond without
//! contacting Symbiont or starting a model; policy belongs to PCP Runtime.
use std::io::{Read, Write};

use anyhow::{Context, Result};
use pcp_runtime::{MaintenanceWorkerRequest, MaintenanceWorkerResponse};

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

fn main() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .context("read legacy PCP maintenance request")?;
    let response = defer_request(&input)?;
    eprintln!("Symbiont PCP maintenance worker is retired; configure a PCP Runtime-owned worker.");
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response).context("write PCP defer response")?;
    stdout.flush().context("flush PCP defer response")
}

fn defer_request(input: &[u8]) -> Result<MaintenanceWorkerResponse> {
    anyhow::ensure!(
        input.len() <= MAX_REQUEST_BYTES,
        "PCP maintenance request exceeds {MAX_REQUEST_BYTES} bytes"
    );
    serde_json::from_slice::<MaintenanceWorkerRequest>(input)
        .context("validate legacy PCP maintenance request")?;
    Ok(MaintenanceWorkerResponse::Defer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_worker_defers_without_a_running_symbiont() {
        let request = MaintenanceWorkerRequest::SelectPacking {
            pages: Vec::new(),
            excluded_candidate_sets: Vec::new(),
        };
        let result = defer_request(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert!(matches!(result, MaintenanceWorkerResponse::Defer));
    }

    #[test]
    fn invalid_or_oversized_requests_are_rejected() {
        assert!(defer_request(b"not-json").is_err());
        assert!(defer_request(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }
}
