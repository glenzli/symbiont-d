//! Symbiont-owned configuration around the official Infer Runtime SDK.
//!
//! The SDK owns Infra Discovery, Core/Capability negotiation, the hardened
//! loopback client, bearer loading, reconnects, and public error decoding.
//! Symbiont retains only its existing credential-store choice and the explicit
//! development endpoint override.

#[cfg(test)]
pub(crate) mod sdk_fixture;

use std::{env, io::Write, path::PathBuf};

use anyhow::{Context, Result};
use infer_runtime_client::{Client, DiscoveryResolver, Error as SdkError};
use tempfile::TempDir;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use crate::secrets::{CredentialStatus, CredentialStore, SecretStore};

const SECRET_ID: &str = "infer-runtime";
const ENDPOINT_OVERRIDE_ENV: &str = "SYMBIONT_INFER_RUNTIME_BASE_URL";

pub(crate) struct InferRuntimeAccess {
    credentials: SecretStore,
    credential_store: RwLock<CredentialStore>,
    resolver: DiscoveryResolver,
    endpoint_source: EndpointSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EndpointSource {
    Environment,
    Discovery,
}

impl EndpointSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Discovery => "discovery",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedConsumerEndpoint {
    pub(crate) base_url: String,
    pub(crate) source: EndpointSource,
    pub(crate) instance_id: String,
    pub(crate) generation: String,
    pub(crate) core_version: String,
}

/// Keeps the private credential bridge alive for every SDK request made by
/// this handle. The SDK reloads the owner-only file when it authenticates.
pub(crate) struct RuntimeClient {
    client: Client,
    _credential_directory: TempDir,
}

impl RuntimeClient {
    pub(crate) fn sdk(&self) -> &Client {
        &self.client
    }
}

impl InferRuntimeAccess {
    pub(crate) async fn open(credential_path: PathBuf) -> Result<Self> {
        let (resolver, endpoint_source) = configured_resolver()?;
        Self::open_with_resolver(credential_path, resolver, endpoint_source).await
    }

    async fn open_with_resolver(
        credential_path: PathBuf,
        resolver: DiscoveryResolver,
        endpoint_source: EndpointSource,
    ) -> Result<Self> {
        Ok(Self {
            credentials: SecretStore::open(credential_path).await?,
            credential_store: RwLock::new(CredentialStore::ConfigFile),
            resolver,
            endpoint_source,
        })
    }

    #[cfg(test)]
    pub(crate) async fn open_for_test(credential_path: PathBuf, endpoint: &str) -> Result<Self> {
        let resolver = DiscoveryResolver::local()
            .with_explicit_endpoint(endpoint.to_owned())
            .context("configure fake Infer Runtime endpoint")?;
        Self::open_with_resolver(credential_path, resolver, EndpointSource::Environment).await
    }

    pub(crate) async fn set_credential_store(&self, store: CredentialStore) {
        *self.credential_store.write().await = store;
    }

    pub(crate) async fn active_credential_store(&self) -> CredentialStore {
        self.credentials
            .active_store(*self.credential_store.read().await)
    }

    pub(crate) async fn debug_credential_override(&self) -> bool {
        self.credentials
            .debug_override(*self.credential_store.read().await)
    }

    pub(crate) async fn credential_status(&self) -> CredentialStatus {
        self.credentials
            .status(SECRET_ID, *self.credential_store.read().await)
            .await
    }

    pub(crate) async fn write_credential(
        &self,
        store: CredentialStore,
        secret: &str,
    ) -> Result<()> {
        self.credentials.write(SECRET_ID, store, secret).await
    }

    /// Builds an authenticated official SDK handle without changing the
    /// existing Symbiont credential or Runtime ACL. The SDK currently accepts
    /// only a raw owner-only file, while Symbiont already supports TOML and
    /// Keychain storage. Bridge the selected value through a process-lifetime
    /// private temporary directory instead of migrating or duplicating the
    /// user's persisted credential.
    pub(crate) async fn client(&self) -> Result<RuntimeClient> {
        let store = *self.credential_store.read().await;
        let token = Zeroizing::new(
            self.credentials
                .read(SECRET_ID, store)
                .await?
                .context("infer-runtime access has not been authorized")?,
        );
        let credential_directory = tempfile::Builder::new()
            .prefix("symbiont-infer-sdk-")
            .tempdir()
            .context("create private Infer Runtime credential bridge")?;
        restrict_directory(credential_directory.path())?;
        let credential_path = credential_directory.path().join("credential");
        write_owner_only(&credential_path, token.as_bytes())?;
        let client = Client::with_discovery(self.resolver.clone())
            .credential_file(&credential_path)
            .build()
            .context("build official Infer Runtime SDK client")?;
        Ok(RuntimeClient {
            client,
            _credential_directory: credential_directory,
        })
    }

    pub(crate) fn resolve_endpoint(&self) -> Result<ResolvedConsumerEndpoint> {
        let endpoint = self
            .resolver
            .resolve()
            .context("resolve Infer Runtime through official SDK discovery")?;
        Ok(ResolvedConsumerEndpoint {
            base_url: endpoint.endpoint,
            source: self.endpoint_source,
            instance_id: endpoint.instance_id,
            generation: endpoint.generation,
            core_version: endpoint.core_version,
        })
    }
}

fn configured_resolver() -> Result<(DiscoveryResolver, EndpointSource)> {
    let resolver = DiscoveryResolver::local();
    let Some(value) = env::var_os(ENDPOINT_OVERRIDE_ENV) else {
        return Ok((resolver, EndpointSource::Discovery));
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow::anyhow!("{ENDPOINT_OVERRIDE_ENV} is not UTF-8"))?;
    if value.trim().is_empty() {
        return Ok((resolver, EndpointSource::Discovery));
    }
    Ok((
        resolver
            .with_explicit_endpoint(value)
            .context("validate explicit Infer Runtime development endpoint")?,
        EndpointSource::Environment,
    ))
}

#[cfg(unix)]
fn restrict_directory(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict private credential bridge {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &std::path::Path) -> Result<()> {
    anyhow::bail!("the official Infer Runtime SDK requires owner-only Unix credential files")
}

#[cfg(unix)]
fn write_owner_only(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private credential bridge {}", path.display()))?;
    file.write_all(bytes)
        .context("write private Infer Runtime credential bridge")?;
    file.flush()
        .context("flush private Infer Runtime credential bridge")
}

#[cfg(not(unix))]
fn write_owner_only(_path: &std::path::Path, _bytes: &[u8]) -> Result<()> {
    anyhow::bail!("the official Infer Runtime SDK requires owner-only Unix credential files")
}

/// Payload-free, user-safe SDK failure classification. In particular, API
/// `error.message` is never surfaced because it may contain Provider detail.
pub(crate) fn sdk_error_summary(error: &SdkError) -> String {
    match error {
        SdkError::Discovery(_) => "infer-runtime discovery is unavailable".to_owned(),
        SdkError::Credential(_) => "infer-runtime credential is unavailable".to_owned(),
        SdkError::Input(_) => "infer-runtime SDK rejected the request shape".to_owned(),
        SdkError::Transport(_) => "infer-runtime transport is unavailable".to_owned(),
        SdkError::ContractMismatch => "infer-runtime contract is incompatible".to_owned(),
        SdkError::Api { status, code, .. } => sdk_api_error_summary(status.as_u16(), code),
        SdkError::MalformedResponse(_) => "infer-runtime returned a malformed response".to_owned(),
    }
}

fn sdk_api_error_summary(status: u16, code: &str) -> String {
    format!("infer-runtime rejected the request (HTTP {status}, {code})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_error_summary_never_exposes_runtime_message() {
        let summary = sdk_api_error_summary(403, "intent_forbidden");
        assert_eq!(
            summary,
            "infer-runtime rejected the request (HTTP 403, intent_forbidden)"
        );
        assert!(!summary.contains("provider body"));
        assert!(!summary.contains("secret path"));
    }

    #[tokio::test]
    async fn sdk_credential_bridge_is_owner_only_and_removed_with_the_handle() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = InferRuntimeAccess::open_for_test(
            temporary.path().join("infer-runtime-secrets.toml"),
            "http://127.0.0.1:18787",
        )
        .await
        .unwrap();
        runtime
            .write_credential(CredentialStore::ConfigFile, "fixture-token")
            .await
            .unwrap();

        let client = runtime.client().await.unwrap();
        let directory = client._credential_directory.path().to_path_buf();
        let credential = directory.join("credential");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(client);
        assert!(!directory.exists());
    }
}
