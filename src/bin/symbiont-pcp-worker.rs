use std::{
    env,
    io::{Read, Write},
    time::Duration,
};

use anyhow::{Context, Result};
use pcp_runtime::{MaintenanceWorkerRequest, MaintenanceWorkerResponse};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:4317/api/internal/pcp-maintenance/evaluate";
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .context("read PCP maintenance request")?;
    anyhow::ensure!(
        input.len() <= MAX_REQUEST_BYTES,
        "PCP maintenance request exceeds {MAX_REQUEST_BYTES} bytes"
    );
    serde_json::from_slice::<MaintenanceWorkerRequest>(&input)
        .context("validate PCP maintenance request")?;

    let endpoint =
        env::var("SYMBIONT_PCP_WORKER_URL").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned());
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(410))
        .build()
        .context("build symbiont semantic worker client")?;
    let response = client
        .post(&endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(input)
        .send()
        .await
        .with_context(|| format!("contact symbiont semantic worker at {endpoint}"))?
        .error_for_status()
        .context("symbiont semantic worker rejected the request")?;
    let body = response
        .bytes()
        .await
        .context("read symbiont semantic worker response")?;
    serde_json::from_slice::<MaintenanceWorkerResponse>(&body)
        .context("validate symbiont semantic worker response")?;
    let mut stdout = std::io::stdout();
    stdout
        .write_all(&body)
        .context("write PCP maintenance response")?;
    stdout.flush().context("flush PCP maintenance response")
}
