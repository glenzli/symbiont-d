use std::{
    env,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use pcp_client::{AccessMode, PcpTenantApi};
use pcp_core::AccessPrincipalType;
use pcp_rpc::{
    BeginEnrollmentParams, EnrollmentClient, EnrollmentClientClaim, EnrollmentPrincipalClaim,
    EnrollmentResult, EnrollmentServiceIdentity, EnrollmentSession, EnrollmentStatusParams,
    OpenEnrollmentSessionParams, PCP_ENROLLMENT_PROTOCOL_ID, PCP_ENROLLMENT_PROTOCOL_VERSION,
    RemotePcpClient, RequestedAccess, RequestedAccessMode,
};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

use crate::continuity::PCP_NAMESPACE;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

pub(super) const HOST_PRINCIPAL_ID: &str = "host:symbiont-d";
const HOST_DISPLAY_NAME: &str = "Symbiont";
const DISCOVERY_SCHEMA: &str = "infra.discovery.registration";
const DISCOVERY_VERSION: &str = "20260812.1";
const UNIX_SOCKET_BINDING: &str = "infra.local.unix-socket";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const STATE_FILE_NAME: &str = "pcp-enrollment-client.json";
static STATE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRegistration {
    schema: String,
    schema_version: String,
    service: EnrollmentServiceIdentity,
    offers: Vec<DiscoveryOffer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryOffer {
    protocol: String,
    protocol_versions: Vec<String>,
    binding: String,
    endpoint: String,
}

#[derive(Clone, Debug)]
struct SelectedEnrollment {
    service: EnrollmentServiceIdentity,
    public_socket: PathBuf,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct EnrollmentState {
    credential: String,
    request_id: Option<String>,
    registration_id: Option<String>,
    service_instance_id: Option<String>,
    generation_registration_id: Option<String>,
    approved_generation: Option<String>,
    reopened_after_generation_change: bool,
    rejected: bool,
}

pub(super) struct ActiveEnrollment {
    pub client: RemotePcpClient,
    pub generation: String,
}

pub(super) enum EnrollmentProbe {
    Active(ActiveEnrollment),
    Pending,
    Rejected,
    Unavailable,
}

/// Owns discovery selection, the client-held enrollment credential, and the
/// recoverable registration identity. Generation-specific sockets never enter
/// durable state.
pub(super) struct EnrollmentManager {
    runtime_root: PathBuf,
    state_path: PathBuf,
    state: Mutex<EnrollmentState>,
    operation: Mutex<()>,
}

impl EnrollmentManager {
    pub async fn open(workspace: &Path) -> Result<Option<Self>> {
        let Some(runtime_root) = runtime_root()? else {
            return Ok(None);
        };
        let state_path = env::var_os("SYMBIONT_PCP_ENROLLMENT_STATE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("data").join(STATE_FILE_NAME));
        Self::open_at(runtime_root, state_path).await.map(Some)
    }

    async fn open_at(runtime_root: PathBuf, state_path: PathBuf) -> Result<Self> {
        let state = load_state(&state_path).await?;
        Ok(Self {
            runtime_root,
            state_path,
            state: Mutex::new(state),
            operation: Mutex::new(()),
        })
    }

    pub async fn probe(&self, preferred_instance: Option<&str>) -> Result<EnrollmentProbe> {
        let _operation = self.operation.lock().await;
        self.probe_locked(preferred_instance).await
    }

    /// Reads only the current discovery generation. The background monitor
    /// uses this before opening a session so a healthy Runtime does not receive
    /// a fresh `open_session` request on every polling interval.
    pub async fn discovered_generation(
        &self,
        preferred_instance: Option<&str>,
    ) -> Result<Option<String>> {
        let stored_instance = self.state.lock().await.service_instance_id.clone();
        let preferred_instance = stored_instance.as_deref().or(preferred_instance);
        let selected = discover_enrollment(&self.runtime_root, preferred_instance)?
            // An identity replacement deliberately invalidates a stored
            // registration. Fall back to unfiltered discovery so probe can
            // clear that state and start a fresh approval request.
            .or(discover_enrollment(&self.runtime_root, None)?);
        Ok(selected.map(|selected| selected.service.generation))
    }

    async fn probe_locked(&self, preferred_instance: Option<&str>) -> Result<EnrollmentProbe> {
        let stored_instance = self.state.lock().await.service_instance_id.clone();
        let preferred_instance = stored_instance.as_deref().or(preferred_instance);
        let Some(selected) = discover_enrollment(&self.runtime_root, preferred_instance)?
            .or(discover_enrollment(&self.runtime_root, None)?)
        else {
            return Ok(EnrollmentProbe::Unavailable);
        };

        self.remember_selected_instance(&selected.service.instance_id)
            .await?;
        let credential = self.ensure_credential().await?;
        let snapshot = self.state.lock().await.clone();
        if snapshot.rejected {
            return Ok(EnrollmentProbe::Rejected);
        }

        let client = EnrollmentClient::new(&selected.public_socket);
        if let Some(registration_id) = snapshot.registration_id {
            match client
                .open_session(OpenEnrollmentSessionParams {
                    registration_id,
                    credential: credential.clone(),
                })
                .await
            {
                Ok(response) => {
                    let access_changed = matches!(
                        &response.result,
                        EnrollmentResult::Active { session }
                            if !session_matches_requested_access(&selected.service, session)
                    );
                    if access_changed {
                        self.clear_registration().await?;
                    } else {
                        return self.handle_result(selected, response.result).await;
                    }
                }
                Err(error) if is_not_found(&error) => {
                    self.clear_registration().await?;
                }
                Err(error) => return Err(error).context("reopen approved PCP registration"),
            }
        }

        let snapshot = self.state.lock().await.clone();
        if let Some(request_id) = snapshot.request_id {
            match client
                .status(EnrollmentStatusParams {
                    request_id,
                    credential: credential.clone(),
                })
                .await
            {
                Ok(response) => return self.handle_result(selected, response.result).await,
                Err(error) if is_not_found(&error) => {
                    self.clear_request().await?;
                }
                Err(error) => return Err(error).context("read PCP enrollment status"),
            }
        }

        let response = client
            .begin(BeginEnrollmentParams {
                client: client_claim(),
                requested_access: requested_access(),
                credential,
            })
            .await
            .context("begin PCP Runtime enrollment")?;
        self.handle_result(selected, response.result).await
    }

    async fn handle_result(
        &self,
        selected: SelectedEnrollment,
        result: EnrollmentResult,
    ) -> Result<EnrollmentProbe> {
        match result {
            EnrollmentResult::Pending { request_id, .. } => {
                let mut state = self.state.lock().await;
                state.request_id = Some(request_id);
                state.rejected = false;
                let snapshot = state.clone();
                drop(state);
                persist_state(&self.state_path, &snapshot).await?;
                Ok(EnrollmentProbe::Pending)
            }
            EnrollmentResult::Rejected { .. } => {
                let mut state = self.state.lock().await;
                state.request_id = None;
                state.rejected = true;
                let snapshot = state.clone();
                drop(state);
                persist_state(&self.state_path, &snapshot).await?;
                Ok(EnrollmentProbe::Rejected)
            }
            EnrollmentResult::Active { session } => {
                anyhow::ensure!(
                    session_matches_requested_access(&selected.service, &session),
                    "PCP enrollment session does not match the current requested access"
                );
                let active =
                    validate_active_session(&self.runtime_root, &selected, &session).await?;
                let mut state = self.state.lock().await;
                state.request_id = None;
                let registration_id = session.registration_id;
                state.registration_id = Some(registration_id.clone());
                state.service_instance_id = Some(session.service.instance_id.clone());
                state.rejected = false;
                if state.generation_registration_id.as_deref() != Some(&registration_id) {
                    state.generation_registration_id = Some(registration_id);
                    state.approved_generation = Some(session.service.generation.clone());
                    state.reopened_after_generation_change = false;
                } else {
                    match state.approved_generation.as_deref() {
                        Some(generation) if generation != session.service.generation => {
                            state.reopened_after_generation_change = true;
                        }
                        None => {
                            state.approved_generation = Some(session.service.generation.clone())
                        }
                        _ => {}
                    }
                }
                let snapshot = state.clone();
                drop(state);
                persist_state(&self.state_path, &snapshot).await?;
                Ok(EnrollmentProbe::Active(active))
            }
        }
    }

    async fn remember_selected_instance(&self, instance_id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(stored) = state.service_instance_id.as_deref() {
            if stored == instance_id {
                return Ok(());
            }

            // A PCP Identity is the tenant boundary.  A discovery-selected
            // replacement must never inherit a registration approved for the
            // previous Identity, but the locally generated enrollment
            // credential remains the same owner-only secret.
            state.request_id = None;
            state.registration_id = None;
            state.generation_registration_id = None;
            state.approved_generation = None;
            state.reopened_after_generation_change = false;
            state.rejected = false;
        }
        state.service_instance_id = Some(instance_id.to_owned());
        let snapshot = state.clone();
        drop(state);
        persist_state(&self.state_path, &snapshot).await
    }

    async fn ensure_credential(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        if valid_credential(&state.credential) {
            return Ok(state.credential.clone());
        }
        anyhow::ensure!(
            state.credential.is_empty(),
            "stored PCP enrollment credential is malformed"
        );
        state.credential = generate_credential()?;
        let credential = state.credential.clone();
        let snapshot = state.clone();
        drop(state);
        persist_state(&self.state_path, &snapshot).await?;
        Ok(credential)
    }

    async fn clear_request(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.request_id = None;
        let snapshot = state.clone();
        drop(state);
        persist_state(&self.state_path, &snapshot).await
    }

    async fn clear_registration(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.registration_id = None;
        state.request_id = None;
        state.generation_registration_id = None;
        state.approved_generation = None;
        state.reopened_after_generation_change = false;
        state.rejected = false;
        let snapshot = state.clone();
        drop(state);
        persist_state(&self.state_path, &snapshot).await
    }
}

fn client_claim() -> EnrollmentClientClaim {
    EnrollmentClientClaim {
        principal: EnrollmentPrincipalClaim {
            principal_id: HOST_PRINCIPAL_ID.to_owned(),
            principal_type: AccessPrincipalType::Host,
            display_name: Some(HOST_DISPLAY_NAME.to_owned()),
        },
    }
}

fn requested_access() -> RequestedAccess {
    RequestedAccess {
        mode: RequestedAccessMode::Contribute,
        scopes: vec![PCP_NAMESPACE.to_owned()],
        allow_cross_scope_derivation: false,
    }
}

fn session_matches_requested_access(
    service: &EnrollmentServiceIdentity,
    session: &EnrollmentSession,
) -> bool {
    let requested = requested_access();
    let mode = match requested.mode {
        RequestedAccessMode::Observe => AccessMode::Observe,
        RequestedAccessMode::Read => AccessMode::Read,
        RequestedAccessMode::Audit => AccessMode::Audit,
        RequestedAccessMode::Contribute => AccessMode::Contribute,
        RequestedAccessMode::Write => AccessMode::Write,
        RequestedAccessMode::Admin => AccessMode::Admin,
    };
    let scopes = requested
        .scopes
        .into_iter()
        .map(|scope| {
            if scope == "user:self" {
                format!("user:{}", service.instance_id)
            } else {
                scope
            }
        })
        .collect();
    let expected = mode.session(
        session.access.principal.clone(),
        session.access.session_id.clone(),
        scopes,
        requested.allow_cross_scope_derivation,
    );
    session.access == expected
}

async fn validate_active_session(
    runtime_root: &Path,
    selected: &SelectedEnrollment,
    session: &EnrollmentSession,
) -> Result<ActiveEnrollment> {
    anyhow::ensure!(
        session.service == selected.service,
        "PCP enrollment session service identity or generation does not match discovery"
    );
    anyhow::ensure!(
        session.binding == UNIX_SOCKET_BINDING,
        "unsupported PCP enrollment session binding: {}",
        session.binding
    );
    anyhow::ensure!(
        session.access.principal.principal_id == HOST_PRINCIPAL_ID
            && session.access.principal.principal_type == AccessPrincipalType::Host,
        "PCP enrollment returned an unexpected principal"
    );
    let socket_path = resolve_unix_endpoint(runtime_root, &session.endpoint)?;
    validate_private_socket(&socket_path)?;
    let client = RemotePcpClient::connect(&socket_path)
        .await
        .context("connect enrolled PCP RPC session")?;
    anyhow::ensure!(
        client.identity_id() == selected.service.instance_id,
        "PCP RPC descriptor identity does not match discovered PCP Identity"
    );
    anyhow::ensure!(
        client.access() == &session.access,
        "PCP RPC descriptor access does not match enrollment session"
    );
    Ok(ActiveEnrollment {
        client,
        generation: selected.service.generation.clone(),
    })
}

fn discover_enrollment(
    runtime_root: &Path,
    preferred_instance: Option<&str>,
) -> Result<Option<SelectedEnrollment>> {
    if !runtime_root.exists() {
        return Ok(None);
    }
    validate_private_directory(runtime_root)?;
    let registrations = runtime_root.join("registrations");
    let sockets = runtime_root.join("sockets");
    validate_private_directory(&registrations)?;
    validate_private_directory(&sockets)?;

    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&registrations).context("scan Infra Discovery registrations")? {
        let entry = entry.context("read Infra Discovery registration entry")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(registration) = read_registration(&path) else {
            continue;
        };
        if !valid_registration(&registration)
            || registration.service.kind != "pcp"
            || entry.file_name().to_string_lossy()
                != format!("pcp--{}.json", registration.service.instance_id)
        {
            continue;
        }
        let Some(offer) = registration.offers.iter().find(|offer| {
            offer.protocol == PCP_ENROLLMENT_PROTOCOL_ID
                && offer
                    .protocol_versions
                    .iter()
                    .any(|version| version == PCP_ENROLLMENT_PROTOCOL_VERSION)
                && offer.binding == UNIX_SOCKET_BINDING
                && valid_unix_endpoint(&offer.endpoint)
        }) else {
            continue;
        };
        let public_socket = resolve_unix_endpoint(runtime_root, &offer.endpoint)?;
        if validate_private_socket(&public_socket).is_err() {
            continue;
        }
        candidates.push(SelectedEnrollment {
            service: registration.service,
            public_socket,
        });
    }
    candidates.sort_by(|left, right| left.service.instance_id.cmp(&right.service.instance_id));

    if let Some(preferred) = preferred_instance {
        return Ok(candidates
            .into_iter()
            .find(|candidate| candidate.service.instance_id == preferred));
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => anyhow::bail!(
            "multiple PCP Runtime registrations are live; configure or retain one Store identity"
        ),
    }
}

fn read_registration(path: &Path) -> Result<DiscoveryRegistration> {
    validate_private_manifest(path)?;
    let bytes = std::fs::read(path)
        .with_context(|| format!("read Infra Discovery manifest {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode Infra Discovery manifest {}", path.display()))
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
        && (offer.binding != UNIX_SOCKET_BINDING || valid_unix_endpoint(&offer.endpoint))
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

fn valid_unix_endpoint(endpoint: &str) -> bool {
    let Some(name) = endpoint.strip_prefix("sockets/") else {
        return false;
    };
    let Some(token) = name.strip_suffix(".sock") else {
        return false;
    };
    !token.is_empty() && token.len() <= 16 && valid_file_token(token)
}

fn resolve_unix_endpoint(runtime_root: &Path, endpoint: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        valid_unix_endpoint(endpoint),
        "invalid Infra Unix socket endpoint"
    );
    let relative = Path::new(endpoint);
    anyhow::ensure!(
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "Infra Unix socket endpoint must be a normal relative path"
    );
    let path = runtime_root.join(relative);
    anyhow::ensure!(
        path.parent() == Some(runtime_root.join("sockets").as_path()),
        "Infra Unix socket endpoint escaped the sockets directory"
    );
    anyhow::ensure!(
        path.as_os_str().as_encoded_bytes().len() < 104,
        "Infra Unix socket endpoint exceeds the supported path length"
    );
    Ok(path)
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

fn generate_credential() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .context("open operating-system random source")?
        .read_exact(&mut bytes)
        .context("generate PCP enrollment credential")?;
    let mut credential = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut credential, "{byte:02x}")
            .expect("writing hexadecimal into a String cannot fail");
    }
    Ok(credential)
}

fn valid_credential(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn load_state(path: &Path) -> Result<EnrollmentState> {
    match fs::symlink_metadata(path).await {
        Ok(_) => validate_private_state_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(EnrollmentState::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect PCP enrollment state {}", path.display()));
        }
    }
    match fs::read(path).await {
        Ok(bytes) => {
            let state: EnrollmentState = serde_json::from_slice(&bytes)
                .with_context(|| format!("decode PCP enrollment state {}", path.display()))?;
            anyhow::ensure!(
                state.credential.is_empty() || valid_credential(&state.credential),
                "stored PCP enrollment credential is malformed"
            );
            Ok(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(EnrollmentState::default())
        }
        Err(error) => {
            Err(error).with_context(|| format!("read PCP enrollment state {}", path.display()))
        }
    }
}

async fn persist_state(path: &Path, state: &EnrollmentState) -> Result<()> {
    let parent = path
        .parent()
        .context("PCP enrollment state path has no parent")?;
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("create PCP enrollment state directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("PCP enrollment state filename is not UTF-8")?;
    let sequence = STATE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let payload = serde_json::to_vec_pretty(state).context("encode PCP enrollment state")?;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .await
        .with_context(|| format!("create PCP enrollment state {}", temporary.display()))?;
    file.write_all(&payload)
        .await
        .context("write PCP enrollment state")?;
    file.sync_all().await.context("sync PCP enrollment state")?;
    drop(file);
    #[cfg(unix)]
    fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
        .await
        .context("secure PCP enrollment state permissions")?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace PCP enrollment state {}", path.display()))?;
    File::open(parent)
        .with_context(|| format!("open PCP enrollment state directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("sync PCP enrollment state directory {}", parent.display()))?;
    Ok(())
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect private directory {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_dir(),
        "{} is not a directory",
        path.display()
    );
    anyhow::ensure!(
        metadata.uid() == unsafe { libc_geteuid() },
        "{} has the wrong owner",
        path.display()
    );
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o700,
        "{} must have mode 0700",
        path.display()
    );
    Ok(())
}

#[cfg(unix)]
fn validate_private_manifest(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect Infra Discovery manifest {}", path.display()))?;
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
fn validate_private_socket(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect Infra Unix socket {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_socket(),
        "Infra endpoint is not a Unix socket"
    );
    anyhow::ensure!(
        metadata.uid() == unsafe { libc_geteuid() },
        "Infra socket has the wrong owner"
    );
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o600,
        "Infra socket must have mode 0600"
    );
    Ok(())
}

#[cfg(unix)]
fn validate_private_state_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect PCP enrollment state {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "PCP enrollment state is not a regular file"
    );
    anyhow::ensure!(
        metadata.uid() == unsafe { libc_geteuid() },
        "PCP enrollment state has the wrong owner"
    );
    anyhow::ensure!(
        metadata.mode() & 0o777 == 0o600,
        "PCP enrollment state must have mode 0600"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_MANIFEST_BYTES,
        "PCP enrollment state exceeds 64 KiB"
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
    anyhow::bail!("Infra Unix socket discovery is unavailable on this platform")
}

#[cfg(not(unix))]
fn validate_private_manifest(_path: &Path) -> Result<()> {
    anyhow::bail!("Infra Unix socket discovery is unavailable on this platform")
}

#[cfg(not(unix))]
fn validate_private_socket(_path: &Path) -> Result<()> {
    anyhow::bail!("Infra Unix socket discovery is unavailable on this platform")
}

#[cfg(not(unix))]
fn validate_private_state_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().starts_with("not_found:"))
}

#[cfg(test)]
mod v08_tests {
    use super::*;
    use pcp_core::{
        IngestPageRequest, PagePayload, Projection, SearchFilters, SearchMode, SearchPagesRequest,
        SearchTermMatch, SourceSpan,
    };

    #[test]
    fn enrollment_requests_contribute_for_the_single_symbiont_scope() {
        let request = requested_access();
        assert_eq!(request.mode, RequestedAccessMode::Contribute);
        assert_eq!(request.scopes, vec![PCP_NAMESPACE.to_owned()]);
        assert!(!request.allow_cross_scope_derivation);
    }

    #[tokio::test]
    async fn a_new_discovered_identity_invalidates_only_the_old_registration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = EnrollmentManager::open_at(
            temporary.path().to_owned(),
            temporary.path().join("client.json"),
        )
        .await
        .expect("open enrollment manager");
        {
            let mut state = manager.state.lock().await;
            state.credential = "ab".repeat(32);
            state.service_instance_id = Some("idn_old".to_owned());
            state.registration_id = Some("reg_old".to_owned());
            state.request_id = Some("req_old".to_owned());
            state.approved_generation = Some("proc_old".to_owned());
            state.generation_registration_id = Some("reg_old".to_owned());
        }

        manager
            .remember_selected_instance("idn_new")
            .await
            .expect("accept new discovery identity");

        let state = manager.state.lock().await;
        assert_eq!(state.credential, "ab".repeat(32));
        assert_eq!(state.service_instance_id.as_deref(), Some("idn_new"));
        assert!(state.registration_id.is_none());
        assert!(state.request_id.is_none());
        assert!(state.approved_generation.is_none());
    }

    /// Opt-in only: this is the release-integration check against an approved
    /// locally discovered Runtime. It writes one idempotent source record, not
    /// a conversation event or any semantic maintenance artifact.
    #[tokio::test]
    #[ignore = "requires an approved local PCP Runtime and writes one idempotent source smoke record"]
    async fn live_contribute_session_can_ingest_search_and_read() {
        let workspace = std::env::current_dir().expect("resolve workspace");
        let manager = EnrollmentManager::open(&workspace)
            .await
            .expect("open enrollment manager")
            .expect("local Infra Discovery must be available");
        let EnrollmentProbe::Active(active) = manager
            .probe(None)
            .await
            .expect("open approved Contribute session")
        else {
            panic!("expected an approved Contribute enrollment");
        };
        let external_event_id = "symbiont-d:integration-smoke:pcp-v08".to_owned();
        let written = active
            .client
            .ingest_page(IngestPageRequest {
                namespace: PCP_NAMESPACE.to_owned(),
                kind: "integration_smoke".to_owned(),
                observed_at: Some("2026-08-15T00:00:00.000Z".to_owned()),
                source_span: Some(SourceSpan {
                    stream_id: "integration-smoke".to_owned(),
                    start: 1,
                    end: 1,
                }),
                payload: Some(PagePayload {
                    media_type: "text/plain".to_owned(),
                    content: "symbiont pcp v0.8 contribute integration smoke".to_owned(),
                }),
                source_refs: Vec::new(),
                based_on_revision_ids: Vec::new(),
                facets: Some(serde_json::json!({"kind": "integration_smoke"})),
                external_event_id: Some(external_event_id),
            })
            .await
            .expect("ingest source-only smoke Page");
        let result = active
            .client
            .search_pages(SearchPagesRequest {
                query: "symbiont pcp v0.8 contribute integration smoke".to_owned(),
                scopes: vec![PCP_NAMESPACE.to_owned()],
                mode: SearchMode::Exact,
                term_match: SearchTermMatch::All,
                // Exact text search must include the payload surface; facets
                // only verifies the structured metadata, not the smoke text.
                projections: vec![Projection::Payload, Projection::Facets],
                filters: SearchFilters::default(),
                limit: 10,
                cursor: None,
            })
            .await
            .expect("search smoke source Page");
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.revision_id == written.revision_id)
        );
        let pages = active
            .client
            .read_pages(pcp_core::ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: vec![written.revision_id.clone()],
                projections: vec![Projection::Payload, Projection::Facets],
                max_chars: 1_024,
            })
            .await
            .expect("read smoke source Page");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].revision.revision_id, written.revision_id);
    }
}

#[cfg(any())]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_client::EmbeddedPcpClient;
    use pcp_rpc::{EnrollmentRequest, EnrollmentResponse, RunningRuntimeEndpoint};
    use pcp_sqlite::SqlitePcpStore;
    use pcp_store::PcpStore;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };

    use super::*;
    use crate::continuity::ContinuityHost;

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos();
        PathBuf::from("/tmp").join(format!(
            "sbe-{}-{}-{nonce}",
            std::process::id(),
            &label[..label.len().min(4)]
        ))
    }

    fn private_dir(path: &Path) {
        std::fs::create_dir_all(path).expect("create private directory");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("secure private directory");
    }

    fn bind_private_socket(path: &Path) -> UnixListener {
        let listener = UnixListener::bind(path).expect("bind private socket");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("secure private socket");
        listener
    }

    fn write_manifest(root: &Path, instance_id: &str, generation: &str, public_endpoint: &str) {
        let registration = DiscoveryRegistration {
            schema: DISCOVERY_SCHEMA.to_owned(),
            schema_version: DISCOVERY_VERSION.to_owned(),
            service: EnrollmentServiceIdentity {
                kind: "pcp".to_owned(),
                instance_id: instance_id.to_owned(),
                generation: generation.to_owned(),
            },
            offers: vec![DiscoveryOffer {
                protocol: PCP_ENROLLMENT_PROTOCOL_ID.to_owned(),
                protocol_versions: vec![PCP_ENROLLMENT_PROTOCOL_VERSION.to_owned()],
                binding: UNIX_SOCKET_BINDING.to_owned(),
                endpoint: public_endpoint.to_owned(),
            }],
        };
        let path = root
            .join("registrations")
            .join(format!("pcp--{instance_id}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec(&registration).expect("encode registration"),
        )
        .expect("write registration");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("secure registration");
    }

    fn spawn_enrollment_server(
        listener: UnixListener,
        expected_operations: Vec<&'static str>,
        responses: Vec<EnrollmentResponse>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            for (expected, response) in expected_operations.into_iter().zip(responses) {
                let (stream, _) = listener.accept().await.expect("accept enrollment request");
                let mut stream = BufReader::new(stream);
                let mut request = Vec::new();
                stream
                    .read_until(b'\n', &mut request)
                    .await
                    .expect("read enrollment request");
                let request: EnrollmentRequest =
                    serde_json::from_slice(&request).expect("decode enrollment request");
                assert_eq!(request.operation, expected);
                if expected == "begin" {
                    let params: BeginEnrollmentParams =
                        request.decode_params("begin").expect("decode begin params");
                    assert_eq!(params.client, client_claim());
                    assert_eq!(params.requested_access, requested_access());
                    assert!(valid_credential(&params.credential));
                }
                let mut payload = serde_json::to_vec(&response).expect("encode response");
                payload.push(b'\n');
                stream
                    .get_mut()
                    .write_all(&payload)
                    .await
                    .expect("write enrollment response");
            }
        })
    }

    #[tokio::test]
    async fn credential_is_generated_once_and_persisted_privately() {
        let root = test_root("credential");
        private_dir(&root);
        let state_path = root.join("client.json");
        let manager = EnrollmentManager::open_at(root.clone(), state_path.clone())
            .await
            .expect("open enrollment manager");
        let first = manager
            .ensure_credential()
            .await
            .expect("generate credential");
        let second = manager.ensure_credential().await.expect("reuse credential");
        assert_eq!(first, second);
        assert!(valid_credential(&first));
        let metadata = std::fs::metadata(&state_path).expect("read state metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let persisted = std::fs::read_to_string(&state_path).expect("read state");
        assert!(persisted.contains(&first));
        assert!(!persisted.contains("sockets/"));
    }

    #[tokio::test]
    async fn begin_status_and_generation_reopen_use_discovery_without_persisting_sockets() {
        let root = test_root("lifecycle");
        private_dir(&root);
        private_dir(&root.join("registrations"));
        private_dir(&root.join("sockets"));
        let state_path = root.join("client.json");

        let store = Arc::new(
            SqlitePcpStore::open(root.join("store.sqlite3"))
                .await
                .expect("open PCP store"),
        );
        let owner_id = store.owner_id().to_owned();
        let principal = ContinuityHost::access_session(&owner_id).principal;
        let access_one = AccessMode::Contribute.session(
            principal,
            "enrolled:reg-test:proc-one",
            vec![PCP_NAMESPACE.to_owned()],
            false,
        );
        let store_api: Arc<dyn PcpStore> = store.clone();
        let rpc_one = RunningRuntimeEndpoint::start(
            root.join("sockets/rpc-one.sock"),
            EmbeddedPcpClient::shared(store_api, access_one.clone()),
        )
        .await
        .expect("start first RPC endpoint");

        let public_one = bind_private_socket(&root.join("sockets/public-one.sock"));
        write_manifest(&root, &owner_id, "proc-one", "sockets/public-one.sock");
        let session_one = EnrollmentSession {
            registration_id: "reg-test".to_owned(),
            service: EnrollmentServiceIdentity {
                kind: "pcp".to_owned(),
                instance_id: owner_id.clone(),
                generation: "proc-one".to_owned(),
            },
            binding: UNIX_SOCKET_BINDING.to_owned(),
            endpoint: "sockets/rpc-one.sock".to_owned(),
            access: access_one.clone(),
        };
        let first_server = spawn_enrollment_server(
            public_one,
            vec!["begin", "status"],
            vec![
                EnrollmentResponse::new(
                    "begin",
                    EnrollmentResult::Pending {
                        request_id: "req-test".to_owned(),
                        requested_at: Utc::now().to_rfc3339(),
                        expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
                    },
                ),
                EnrollmentResponse::new(
                    "status",
                    EnrollmentResult::Active {
                        session: session_one,
                    },
                ),
            ],
        );

        let manager = EnrollmentManager::open_at(root.clone(), state_path.clone())
            .await
            .expect("open enrollment manager");
        assert!(matches!(
            manager
                .probe(Some(&owner_id))
                .await
                .expect("begin enrollment"),
            EnrollmentProbe::Pending
        ));
        let EnrollmentProbe::Active(active_one) = manager
            .probe(Some(&owner_id))
            .await
            .expect("read approved enrollment")
        else {
            panic!("status did not activate the enrollment");
        };
        assert_eq!(active_one.generation, "proc-one");
        first_server.await.expect("finish first enrollment server");

        let mut access_two = access_one;
        access_two.session_id = "enrolled:reg-test:proc-two".to_owned();
        let store_api: Arc<dyn PcpStore> = store;
        let rpc_two = RunningRuntimeEndpoint::start(
            root.join("sockets/rpc-two.sock"),
            EmbeddedPcpClient::shared(store_api, access_two.clone()),
        )
        .await
        .expect("start second RPC endpoint");
        let public_two = bind_private_socket(&root.join("sockets/public-two.sock"));
        write_manifest(&root, &owner_id, "proc-two", "sockets/public-two.sock");
        let second_server = spawn_enrollment_server(
            public_two,
            vec!["open_session"],
            vec![EnrollmentResponse::new(
                "open_session",
                EnrollmentResult::Active {
                    session: EnrollmentSession {
                        registration_id: "reg-test".to_owned(),
                        service: EnrollmentServiceIdentity {
                            kind: "pcp".to_owned(),
                            instance_id: owner_id.clone(),
                            generation: "proc-two".to_owned(),
                        },
                        binding: UNIX_SOCKET_BINDING.to_owned(),
                        endpoint: "sockets/rpc-two.sock".to_owned(),
                        access: access_two,
                    },
                },
            )],
        );
        let EnrollmentProbe::Active(active_two) = manager
            .probe(Some(&owner_id))
            .await
            .expect("reopen registration")
        else {
            panic!("open_session did not activate the new generation");
        };
        assert_eq!(active_two.generation, "proc-two");
        second_server
            .await
            .expect("finish second enrollment server");

        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).expect("read persisted state"))
                .expect("decode persisted state");
        assert_eq!(persisted["registration_id"], "reg-test");
        assert_eq!(persisted["generation_registration_id"], "reg-test");
        assert_eq!(persisted["approved_generation"], "proc-one");
        assert_eq!(persisted["reopened_after_generation_change"], true);
        assert!(persisted.get("endpoint").is_none());

        rpc_one.shutdown().await;
        rpc_two.shutdown().await;
    }

    #[test]
    fn endpoint_resolution_rejects_traversal_and_absolute_paths() {
        let root = Path::new("/private/runtime");
        assert!(resolve_unix_endpoint(root, "sockets/pcp-a.sock").is_ok());
        assert!(resolve_unix_endpoint(root, "sockets/../pcp-a.sock").is_err());
        assert!(resolve_unix_endpoint(root, "/sockets/pcp-a.sock").is_err());
        assert!(resolve_unix_endpoint(root, "other/pcp-a.sock").is_err());
    }

    #[test]
    fn enrollment_requests_the_scopes_owned_by_continuity() {
        assert_eq!(requested_access().scopes, vec![PCP_NAMESPACE.to_owned()]);
    }

    #[test]
    fn enrollment_rejects_a_registration_with_stale_scope_grants() {
        let owner_id = "usr-test";
        let service = EnrollmentServiceIdentity {
            kind: "pcp".to_owned(),
            instance_id: owner_id.to_owned(),
            generation: "proc-test".to_owned(),
        };
        let principal = ContinuityHost::access_session(owner_id).principal;
        let access = |scope: &str, allow_cross_scope_derivation: bool| {
            AccessMode::Contribute.session(
                principal.clone(),
                "enrolled:reg-test:proc-test",
                vec![scope.to_owned()],
                allow_cross_scope_derivation,
            )
        };
        let session = |access| EnrollmentSession {
            registration_id: "reg-test".to_owned(),
            service: service.clone(),
            binding: UNIX_SOCKET_BINDING.to_owned(),
            endpoint: "sockets/session.sock".to_owned(),
            access,
        };

        assert!(session_matches_requested_access(
            &service,
            &session(access(PCP_NAMESPACE, false))
        ));
        assert!(!session_matches_requested_access(
            &service,
            &session(access("conversation:symbiont-d", false))
        ));
        assert!(!session_matches_requested_access(
            &service,
            &session(access(PCP_NAMESPACE, true))
        ));
    }

    #[tokio::test]
    async fn replacing_a_registration_resets_its_generation_provenance() {
        let root = test_root("registration-generation-reset");
        private_dir(&root);
        let state_path = root.join("client.json");
        let manager = EnrollmentManager::open_at(root, state_path.clone())
            .await
            .expect("open enrollment manager");
        {
            let mut state = manager.state.lock().await;
            state.registration_id = Some("reg-stale".to_owned());
            state.request_id = Some("req-stale".to_owned());
            state.generation_registration_id = Some("reg-stale".to_owned());
            state.approved_generation = Some("proc-old".to_owned());
            state.reopened_after_generation_change = true;
            state.rejected = true;
        }

        manager
            .clear_registration()
            .await
            .expect("clear stale registration");

        let persisted: EnrollmentState =
            serde_json::from_slice(&std::fs::read(state_path).expect("read persisted state"))
                .expect("decode persisted state");
        assert_eq!(persisted.registration_id, None);
        assert_eq!(persisted.request_id, None);
        assert_eq!(persisted.generation_registration_id, None);
        assert_eq!(persisted.approved_generation, None);
        assert!(!persisted.reopened_after_generation_change);
        assert!(!persisted.rejected);
    }

    #[test]
    fn discovery_registration_has_no_liveness_lease() {
        let registration = DiscoveryRegistration {
            schema: DISCOVERY_SCHEMA.to_owned(),
            schema_version: DISCOVERY_VERSION.to_owned(),
            service: EnrollmentServiceIdentity {
                kind: "pcp".to_owned(),
                instance_id: "usr-test".to_owned(),
                generation: "proc-test".to_owned(),
            },
            offers: vec![DiscoveryOffer {
                protocol: PCP_ENROLLMENT_PROTOCOL_ID.to_owned(),
                protocol_versions: vec![PCP_ENROLLMENT_PROTOCOL_VERSION.to_owned()],
                binding: UNIX_SOCKET_BINDING.to_owned(),
                endpoint: "sockets/0123456789ABCDEF.sock".to_owned(),
            }],
        };
        assert!(valid_registration(&registration));
        assert!(
            serde_json::to_value(registration)
                .expect("encode discovery registration")
                .get("lease")
                .is_none()
        );
        assert!(!valid_unix_endpoint("sockets/0123456789ABCDEFG.sock"));
    }
}
