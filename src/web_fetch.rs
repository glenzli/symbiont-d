use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use reqwest::{
    Client, Url,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use serde::Serialize;
use serde_json::json;
use tokio::net::lookup_host;

use crate::permission::{
    PermissionBroker, PermissionDecision, PermissionRequestDraft, PermissionResolutionSource,
};

const MAX_REDIRECTS: usize = 5;
const MAX_RESPONSE_BYTES: usize = 1_000_000;
const MAX_URL_CHARS: usize = 4_096;

#[derive(Clone)]
pub struct WebFetcher {
    permissions: Arc<PermissionBroker>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchDocument {
    pub requested_url: String,
    pub final_url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub byte_size: usize,
    pub truncated: bool,
    pub text: String,
}

impl WebFetcher {
    pub fn new(permissions: Arc<PermissionBroker>) -> Result<Self> {
        Ok(Self { permissions })
    }

    pub async fn fetch(&self, value: &str, purpose: &str, origin: &str) -> Result<FetchDocument> {
        let requested_url = value.trim();
        if requested_url.is_empty() || requested_url.chars().count() > MAX_URL_CHARS {
            anyhow::bail!("fetch URL must contain between 1 and {MAX_URL_CHARS} characters");
        }
        let mut current = Url::parse(requested_url).context("parse fetch URL")?;
        let mut approved_targets = HashSet::new();

        for redirect_count in 0..=MAX_REDIRECTS {
            let target = validate_public_target(&current).await?;
            if approved_targets.insert(target.session_key.clone()) {
                let resolution = self
                    .permissions
                    .request(PermissionRequestDraft {
                        kind: "networkAccess".to_owned(),
                        source: "symbiont".to_owned(),
                        origin: origin.to_owned(),
                        title: format!("允许读取 {}", target.host),
                        reason: Some(purpose.trim().to_owned()),
                        command: None,
                        cwd: None,
                        host: Some(target.host.clone()),
                        protocol: Some(target.scheme.clone()),
                        details: json!({
                            "url": current.as_str(),
                            "port": target.port,
                            "maximumResponseBytes": MAX_RESPONSE_BYTES
                        }),
                        allow_accept: true,
                        allow_session: true,
                        allow_cancel: false,
                        session_key: Some(target.session_key.clone()),
                        timeout: None,
                    })
                    .await;
                if !matches!(
                    resolution.decision,
                    PermissionDecision::Accept | PermissionDecision::AcceptForSession
                ) {
                    anyhow::bail!(
                        "network access to {} was declined by the Host ({:?})",
                        target.host,
                        resolution.source
                    );
                }
                if resolution.source == PermissionResolutionSource::BackgroundPolicy {
                    anyhow::bail!("background network access requires a prior session grant");
                }
            }

            let client = client_for_target(&target)?;
            let mut response = client
                .get(current.clone())
                .header(
                    ACCEPT,
                    "text/html,application/xhtml+xml,application/json,text/plain,application/xml;q=0.9,*/*;q=0.1",
                )
                .send()
                .await
                .with_context(|| format!("fetch {}", current.as_str()))?;
            if response.status().is_redirection() {
                if redirect_count == MAX_REDIRECTS {
                    anyhow::bail!("fetch exceeded {MAX_REDIRECTS} redirects");
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .context("redirect omitted Location header")?
                    .to_str()
                    .context("redirect Location is not valid text")?;
                current = current.join(location).context("resolve redirect URL")?;
                continue;
            }
            if !response.status().is_success() {
                anyhow::bail!(
                    "fetch returned HTTP {} for {}",
                    response.status().as_u16(),
                    current.as_str()
                );
            }

            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if !is_textual_content_type(content_type.as_deref()) {
                anyhow::bail!(
                    "fetch refused non-text content type {}",
                    content_type.as_deref().unwrap_or("unknown")
                );
            }
            if let Some(length) = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
                && length > MAX_RESPONSE_BYTES
            {
                anyhow::bail!(
                    "fetch response declares {length} bytes, above the {MAX_RESPONSE_BYTES}-byte limit"
                );
            }

            let status = response.status().as_u16();
            let mut body = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = response.chunk().await.context("read fetch response body")? {
                let remaining = MAX_RESPONSE_BYTES.saturating_sub(body.len());
                if chunk.len() > remaining {
                    body.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&body).into_owned();
            return Ok(FetchDocument {
                requested_url: requested_url.to_owned(),
                final_url: current.to_string(),
                status,
                content_type,
                byte_size: body.len(),
                truncated,
                text,
            });
        }
        unreachable!("redirect loop returns or fails within its bounded range")
    }
}

struct ValidatedTarget {
    host: String,
    scheme: String,
    port: u16,
    session_key: String,
    addresses: Vec<SocketAddr>,
}

async fn validate_public_target(url: &Url) -> Result<ValidatedTarget> {
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("fetch only supports http and https URLs");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("fetch URLs cannot contain credentials");
    }
    let host = url.host_str().context("fetch URL omitted a host")?;
    let port = url
        .port_or_known_default()
        .context("fetch URL omitted a port")?;
    let allowed_port = match url.scheme() {
        "http" => port == 80,
        "https" => port == 443,
        _ => false,
    };
    if !allowed_port {
        anyhow::bail!("fetch only permits the standard port for its URL scheme");
    }

    let addresses = lookup_host((host, port))
        .await
        .with_context(|| format!("resolve fetch host {host}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        anyhow::bail!("fetch host {host} resolved to no addresses");
    }
    if addresses.iter().any(|address| !is_public_ip(address.ip())) {
        anyhow::bail!("fetch refuses private, local, or special-use network targets");
    }

    let normalized_host = host.to_ascii_lowercase();
    let scheme = url.scheme().to_owned();
    Ok(ValidatedTarget {
        session_key: format!("network:{scheme}:{normalized_host}:{port}"),
        host: normalized_host,
        scheme,
        port,
        addresses,
    })
}

fn client_for_target(target: &ValidatedTarget) -> Result<Client> {
    Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(25))
        .user_agent(concat!("symbiont-d/", env!("CARGO_PKG_VERSION")))
        .resolve_to_addrs(&target.host, &target.addresses)
        .build()
        .context("build pinned web fetch client")
}

fn is_textual_content_type(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return true;
    };
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type.starts_with("text/")
        || media_type == "application/json"
        || media_type.ends_with("+json")
        || media_type == "application/xml"
        || media_type.ends_with("+xml")
        || media_type == "application/xhtml+xml"
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_unspecified()
        || a == 0
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    !(address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{is_public_ip, is_textual_content_type};

    #[test]
    fn private_and_special_addresses_are_rejected() {
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn only_textual_response_types_are_accepted() {
        assert!(is_textual_content_type(Some("text/html; charset=utf-8")));
        assert!(is_textual_content_type(Some("application/problem+json")));
        assert!(!is_textual_content_type(Some("image/png")));
    }
}
