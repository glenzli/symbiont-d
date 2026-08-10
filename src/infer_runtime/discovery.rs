use std::{
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};

const DISCOVERY_SCHEMA: &str = "infra.discovery.registration";
const DISCOVERY_VERSION: &str = "20260810.1";
const SERVICE_KIND: &str = "infer-runtime";
const DEFAULT_INSTANCE_ID: &str = "local";
const CONSUMER_PROTOCOL: &str = "infer-runtime.consumer";
const CONSUMER_PROTOCOL_VERSION: &str = "0.1.0-candidate.2";
const HTTP_LOOPBACK_BINDING: &str = "infer-runtime.http-loopback";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRegistration {
    schema: String,
    schema_version: String,
    service: DiscoveryService,
    lease: DiscoveryLease,
    offers: Vec<DiscoveryOffer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryService {
    kind: String,
    instance_id: String,
    generation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryLease {
    renewed_at: String,
    expires_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryOffer {
    protocol: String,
    protocol_versions: Vec<String>,
    binding: String,
    endpoint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiscoveredConsumer {
    pub base_url: String,
    pub instance_id: String,
    pub generation: String,
    pub expires_at: DateTime<Utc>,
}

pub(super) fn discover_consumer(now: DateTime<Utc>) -> Result<Option<DiscoveredConsumer>> {
    let Some(root) = runtime_root()? else {
        return Ok(None);
    };
    discover_consumer_at(&root, now)
}

fn discover_consumer_at(root: &Path, now: DateTime<Utc>) -> Result<Option<DiscoveredConsumer>> {
    if !root.exists() {
        return Ok(None);
    }
    validate_private_directory(root)?;
    let registrations = root.join("registrations");
    let sockets = root.join("sockets");
    validate_private_directory(&registrations)?;
    validate_private_directory(&sockets)?;

    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&registrations).context("scan Infra registrations")? {
        let entry = entry.context("read Infra registration entry")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(registration) = read_registration(&path) else {
            continue;
        };
        if !valid_registration(&registration)
            || registration.service.kind != SERVICE_KIND
            || entry.file_name().to_string_lossy()
                != format!("{SERVICE_KIND}--{}.json", registration.service.instance_id)
        {
            continue;
        }
        let Some(expires_at) = live_lease(&registration.lease, now) else {
            continue;
        };
        let Some(offer) = registration.offers.iter().find(|offer| {
            offer.protocol == CONSUMER_PROTOCOL
                && offer
                    .protocol_versions
                    .iter()
                    .any(|version| version == CONSUMER_PROTOCOL_VERSION)
                && offer.binding == HTTP_LOOPBACK_BINDING
                && canonical_loopback_origin(&offer.endpoint).is_ok()
        }) else {
            continue;
        };
        candidates.push(DiscoveredConsumer {
            base_url: canonical_loopback_origin(&offer.endpoint)?,
            instance_id: registration.service.instance_id,
            generation: registration.service.generation,
            expires_at,
        });
    }
    candidates.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.instance_id == DEFAULT_INSTANCE_ID)
    {
        return Ok(Some(candidates.remove(index)));
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => anyhow::bail!(
            "multiple infer-runtime Consumer registrations are live and none is the local instance"
        ),
    }
}

pub(super) fn canonical_loopback_origin(value: &str) -> Result<String> {
    let raw = value.trim();
    anyhow::ensure!(
        raw == value,
        "infer-runtime address contains surrounding whitespace"
    );
    anyhow::ensure!(
        !raw.ends_with('/'),
        "infer-runtime address must not end with /"
    );
    let url = Url::parse(raw).context("parse infer-runtime address")?;
    anyhow::ensure!(
        url.scheme() == "http",
        "infer-runtime address must use http"
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "infer-runtime address must not contain user info"
    );
    anyhow::ensure!(
        url.path() == "/" && url.query().is_none() && url.fragment().is_none(),
        "infer-runtime address must be an origin without path, query, or fragment"
    );
    let port = url
        .port()
        .filter(|port| *port != 0)
        .context("infer-runtime address must include a non-zero port")?;
    let host = url
        .host_str()
        .context("infer-runtime address has no host")?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<IpAddr>()
        .context("infer-runtime address must use a numeric IP address")?;
    anyhow::ensure!(host.is_loopback(), "infer-runtime address must be loopback");
    let canonical = match host {
        IpAddr::V4(host) => format!("http://{host}:{port}"),
        IpAddr::V6(host) => format!("http://[{host}]:{port}"),
    };
    anyhow::ensure!(raw == canonical, "infer-runtime address is not canonical");
    Ok(canonical)
}

fn read_registration(path: &Path) -> Result<DiscoveryRegistration> {
    validate_private_manifest(path)?;
    let bytes =
        std::fs::read(path).with_context(|| format!("read Infra manifest {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode Infra manifest {}", path.display()))
}

fn live_lease(lease: &DiscoveryLease, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if lease.renewed_at.len() > 40 || lease.expires_at.len() > 40 {
        return None;
    }
    let renewed_at = DateTime::parse_from_rfc3339(&lease.renewed_at)
        .ok()?
        .with_timezone(&Utc);
    let expires_at = DateTime::parse_from_rfc3339(&lease.expires_at)
        .ok()?
        .with_timezone(&Utc);
    (renewed_at < expires_at
        && expires_at - renewed_at <= chrono::Duration::seconds(120)
        && renewed_at <= now + chrono::Duration::seconds(15)
        && expires_at <= now + chrono::Duration::seconds(120)
        && expires_at > now)
        .then_some(expires_at)
}

fn valid_registration(registration: &DiscoveryRegistration) -> bool {
    registration.schema == DISCOVERY_SCHEMA
        && registration.schema_version == DISCOVERY_VERSION
        && valid_service_kind(&registration.service.kind)
        && valid_file_token(&registration.service.instance_id)
        && valid_file_token(&registration.service.generation)
        && (1..=64).contains(&registration.offers.len())
        && registration.offers.iter().all(valid_offer)
}

fn valid_offer(offer: &DiscoveryOffer) -> bool {
    (1..=128).contains(&offer.protocol.len())
        && valid_contract_id(&offer.protocol)
        && (1..=128).contains(&offer.binding.len())
        && valid_contract_id(&offer.binding)
        && (1..=16).contains(&offer.protocol_versions.len())
        && offer
            .protocol_versions
            .iter()
            .all(|version| (1..=64).contains(&version.len()) && valid_contract_version(version))
        && offer
            .protocol_versions
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == offer.protocol_versions.len()
        && (1..=512).contains(&offer.endpoint.len())
}

fn valid_service_kind(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.split(['.', '-']).all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_contract_id(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'+' | b'/' | b'@' | b'%' | b'-')
        })
}

fn valid_contract_version(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'+' | b'-')
        })
}

fn valid_file_token(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn runtime_root() -> Result<Option<PathBuf>> {
    if let Some(override_path) = env::var_os("INFRA_PROTOCOL_RUNTIME_DIR") {
        let path = PathBuf::from(override_path);
        anyhow::ensure!(
            path.is_absolute(),
            "INFRA_PROTOCOL_RUNTIME_DIR must be absolute"
        );
        return Ok(Some(path));
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .output()
            .context("obtain the Darwin user temporary directory")?;
        anyhow::ensure!(
            output.status.success(),
            "getconf DARWIN_USER_TEMP_DIR failed"
        );
        let base = String::from_utf8(output.stdout).context("DARWIN_USER_TEMP_DIR is not UTF-8")?;
        let base = PathBuf::from(base.trim());
        anyhow::ensure!(base.is_absolute(), "DARWIN_USER_TEMP_DIR must be absolute");
        return Ok(Some(base.join("infra-protocol")));
    }
    #[cfg(target_os = "linux")]
    {
        let Some(base) = env::var_os("XDG_RUNTIME_DIR") else {
            return Ok(None);
        };
        let base = PathBuf::from(base);
        anyhow::ensure!(base.is_absolute(), "XDG_RUNTIME_DIR must be absolute");
        return Ok(Some(base.join("infra-protocol")));
    }
    #[allow(unreachable_code)]
    Ok(None)
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect Infra directory {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_dir(),
        "Infra path is not a directory"
    );
    anyhow::ensure!(
        metadata.uid() == unsafe { libc_geteuid() },
        "Infra directory has the wrong owner"
    );
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o700,
        "Infra directory must have mode 0700"
    );
    Ok(())
}

#[cfg(unix)]
fn validate_private_manifest(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect Infra manifest {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "Infra manifest is not a regular file"
    );
    anyhow::ensure!(
        metadata.uid() == unsafe { libc_geteuid() },
        "Infra manifest has the wrong owner"
    );
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o600,
        "Infra manifest must have mode 0600"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_MANIFEST_BYTES,
        "Infra manifest exceeds 64 KiB"
    );
    Ok(())
}

#[cfg(unix)]
unsafe fn libc_geteuid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(not(unix))]
fn validate_private_directory(_path: &Path) -> Result<()> {
    anyhow::bail!("Infra Discovery ownership validation is unavailable on this platform")
}

#[cfg(not(unix))]
fn validate_private_manifest(_path: &Path) -> Result<()> {
    anyhow::bail!("Infra Discovery ownership validation is unavailable on this platform")
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn private_dir(path: &Path) {
        std::fs::create_dir_all(path).expect("create private directory");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("secure private directory");
    }

    fn write_registration(
        root: &Path,
        lease: DiscoveryLease,
        protocol_version: &str,
        binding: &str,
        endpoint: &str,
    ) {
        let registration = DiscoveryRegistration {
            schema: DISCOVERY_SCHEMA.to_owned(),
            schema_version: DISCOVERY_VERSION.to_owned(),
            service: DiscoveryService {
                kind: SERVICE_KIND.to_owned(),
                instance_id: DEFAULT_INSTANCE_ID.to_owned(),
                generation: "gen-test".to_owned(),
            },
            lease,
            offers: vec![DiscoveryOffer {
                protocol: CONSUMER_PROTOCOL.to_owned(),
                protocol_versions: vec![protocol_version.to_owned()],
                binding: binding.to_owned(),
                endpoint: endpoint.to_owned(),
            }],
        };
        let path = root.join("registrations").join("infer-runtime--local.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&registration).expect("encode registration"),
        )
        .expect("write registration");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("secure registration");
    }

    #[test]
    fn accepts_only_canonical_numeric_loopback_origins() {
        for valid in [
            "http://127.0.0.1:8787",
            "http://127.0.0.2:1",
            "http://[::1]:8787",
        ] {
            assert_eq!(canonical_loopback_origin(valid).unwrap(), valid);
        }
        for invalid in [
            "http://localhost:8787",
            "https://127.0.0.1:8787",
            "http://0.0.0.0:8787",
            "http://192.168.1.2:8787",
            "http://127.0.0.1",
            "http://127.0.0.1:8787/",
            "http://127.0.0.1:8787/path",
            "http://user@127.0.0.1:8787",
            "http://127.0.0.1:8787?q=1",
            " http://127.0.0.1:8787",
        ] {
            assert!(canonical_loopback_origin(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn selects_a_live_exact_consumer_offer_and_ignores_an_expired_one() {
        let root = std::env::temp_dir().join(format!(
            "symbiont-infer-discovery-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        private_dir(&root);
        private_dir(&root.join("registrations"));
        private_dir(&root.join("sockets"));
        let now = Utc::now();
        write_registration(
            &root,
            DiscoveryLease {
                renewed_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
                expires_at: (now + chrono::Duration::seconds(44)).to_rfc3339(),
            },
            CONSUMER_PROTOCOL_VERSION,
            HTTP_LOOPBACK_BINDING,
            "http://127.0.0.1:8787",
        );
        let selected = discover_consumer_at(&root, now)
            .expect("discover Consumer")
            .expect("select Consumer");
        assert_eq!(selected.base_url, "http://127.0.0.1:8787");
        assert_eq!(selected.instance_id, DEFAULT_INSTANCE_ID);
        assert_eq!(selected.generation, "gen-test");

        write_registration(
            &root,
            DiscoveryLease {
                renewed_at: (now - chrono::Duration::seconds(60)).to_rfc3339(),
                expires_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
            },
            CONSUMER_PROTOCOL_VERSION,
            HTTP_LOOPBACK_BINDING,
            "http://127.0.0.1:8787",
        );
        assert_eq!(discover_consumer_at(&root, now).unwrap(), None);
    }

    #[test]
    fn requires_the_exact_consumer_protocol_version_and_binding() {
        let root = std::env::temp_dir().join(format!(
            "symbiont-infer-contract-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        private_dir(&root);
        private_dir(&root.join("registrations"));
        private_dir(&root.join("sockets"));
        let now = Utc::now();
        let lease = || DiscoveryLease {
            renewed_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(44)).to_rfc3339(),
        };

        write_registration(
            &root,
            lease(),
            "0.1.0-candidate",
            HTTP_LOOPBACK_BINDING,
            "http://127.0.0.1:8787",
        );
        assert_eq!(discover_consumer_at(&root, now).unwrap(), None);

        write_registration(
            &root,
            lease(),
            CONSUMER_PROTOCOL_VERSION,
            "infra.local.http",
            "http://127.0.0.1:8787",
        );
        assert_eq!(discover_consumer_at(&root, now).unwrap(), None);
    }
}
