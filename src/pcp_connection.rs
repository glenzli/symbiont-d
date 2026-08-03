use std::{env, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use pcp_client::{EmbeddedPcpClient, PcpApi};
use pcp_rpc::RemotePcpClient;
use pcp_sqlite::SqlitePcpStore;
use pcp_store::PcpStore;
use tokio::time::Instant;

use crate::continuity::ContinuityHost;

const HOST_PRINCIPAL_ID: &str = "host:symbiont-d";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);

pub async fn open(workspace: &Path) -> Result<Arc<dyn PcpApi>> {
    let Some(socket_path) = env::var_os("SYMBIONT_PCP_RUNTIME_SOCKET") else {
        return open_embedded(workspace).await;
    };
    let socket_path = std::path::PathBuf::from(socket_path);
    let timeout = runtime_connect_timeout()?;
    let started = Instant::now();
    loop {
        match RemotePcpClient::connect_expected(&socket_path, HOST_PRINCIPAL_ID).await {
            Ok(client) => return Ok(Arc::new(client)),
            Err(error) if started.elapsed() >= timeout => {
                return Err(error).with_context(|| {
                    format!(
                        "connect configured PCP runtime at {} within {} ms; embedded fallback is disabled",
                        socket_path.display(),
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
        .map(std::path::PathBuf::from)
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
