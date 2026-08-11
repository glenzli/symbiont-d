//! Shared local Consumer access to infer-runtime.
//!
//! This module owns endpoint discovery, bearer credentials, and the hardened
//! loopback HTTP client. Product capabilities such as voice transcription and
//! generic semantic inference build their own request contracts on top of it.

mod contract;
mod discovery;

use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Client, Url, redirect::Policy};
use tokio::sync::RwLock;

use crate::secrets::{CredentialStatus, CredentialStore, SecretStore};
pub(crate) use contract::InferenceWorkload;
use discovery::{DiscoveredConsumer, canonical_loopback_origin, discover_consumer};

const SECRET_ID: &str = "infer-runtime";
const ENDPOINT_OVERRIDE_ENV: &str = "SYMBIONT_INFER_RUNTIME_BASE_URL";
const COMPATIBILITY_FALLBACK: &str = "http://127.0.0.1:8787";

pub(crate) struct InferRuntimeAccess {
    credentials: SecretStore,
    credential_store: RwLock<CredentialStore>,
    client: Client,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EndpointSource {
    Environment,
    Discovery,
    CompatibilityFallback,
}

impl EndpointSource {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Discovery => "discovery",
            Self::CompatibilityFallback => "compatibility_fallback",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedConsumerEndpoint {
    pub(crate) base_url: String,
    pub(crate) source: EndpointSource,
    pub(crate) instance_id: Option<String>,
    pub(crate) generation: Option<String>,
    pub(crate) lease_expires_at: Option<DateTime<Utc>>,
}

impl ResolvedConsumerEndpoint {
    fn discovered(selection: DiscoveredConsumer) -> Self {
        Self {
            base_url: selection.base_url,
            source: EndpointSource::Discovery,
            instance_id: Some(selection.instance_id),
            generation: Some(selection.generation),
            lease_expires_at: Some(selection.expires_at),
        }
    }
}

pub(crate) struct RuntimeConnection {
    pub(crate) endpoint: ResolvedConsumerEndpoint,
    pub(crate) token: String,
}

impl InferRuntimeAccess {
    pub(crate) async fn open(credential_path: PathBuf) -> Result<Self> {
        Ok(Self {
            credentials: SecretStore::open(credential_path).await?,
            credential_store: RwLock::new(CredentialStore::ConfigFile),
            client: Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .build()?,
        })
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
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

    pub(crate) async fn connection(&self) -> Result<RuntimeConnection> {
        let store = *self.credential_store.read().await;
        let token = self
            .credentials
            .read(SECRET_ID, store)
            .await?
            .context("infer-runtime access has not been authorized")?;
        Ok(RuntimeConnection {
            endpoint: self.resolve_endpoint()?,
            token,
        })
    }

    pub(crate) fn resolve_endpoint(&self) -> Result<ResolvedConsumerEndpoint> {
        resolve_endpoint()
    }
}

pub(crate) fn endpoint_url(base_url: &str, path: &str) -> Result<Url> {
    let base_url = canonical_loopback_origin(base_url)?;
    anyhow::ensure!(path.starts_with('/'), "infer-runtime path must be absolute");
    Url::parse(&format!("{base_url}{path}")).context("build infer-runtime endpoint")
}

fn resolve_endpoint() -> Result<ResolvedConsumerEndpoint> {
    if let Some(value) = env::var_os(ENDPOINT_OVERRIDE_ENV) {
        let value = value
            .into_string()
            .map_err(|_| anyhow::anyhow!("{ENDPOINT_OVERRIDE_ENV} is not UTF-8"))?;
        if !value.trim().is_empty() {
            return Ok(ResolvedConsumerEndpoint {
                base_url: canonical_loopback_origin(&value)?,
                source: EndpointSource::Environment,
                instance_id: None,
                generation: None,
                lease_expires_at: None,
            });
        }
    }
    if let Some(discovered) = discover_consumer(Utc::now())? {
        return Ok(ResolvedConsumerEndpoint::discovered(discovered));
    }
    Ok(ResolvedConsumerEndpoint {
        base_url: COMPATIBILITY_FALLBACK.to_owned(),
        source: EndpointSource::CompatibilityFallback,
        instance_id: None,
        generation: None,
        lease_expires_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_canonical_loopback_runtime_addresses() {
        assert!(endpoint_url("http://127.0.0.1:8787", "/v1/responses").is_ok());
        assert!(endpoint_url("http://localhost:8787", "/v1/responses").is_err());
        assert!(endpoint_url("http://127.0.0.1:8787/api", "/v1/responses").is_err());
        assert!(endpoint_url("https://example.com", "/v1/responses").is_err());
    }
}
