//! Private, read-only IMAP intake for external research reports and alerts.
//!
//! This module owns the mailbox protocol, credentials, cursor and failure
//! policy. It deliberately emits only short-lived sensing candidates: e-mail
//! is an external input surface, never a memory-writing surface.

mod normalization;

use std::{
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use async_imap::{Client as ImapClient, Session as ImapSession};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::StreamExt;
use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    net::TcpStream,
    sync::{Mutex, RwLock, oneshot, watch},
    time::timeout,
};
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};

use self::normalization::MailDocument;
use crate::{
    secrets::{CredentialStatus, CredentialStore, SecretStore},
    sensing::{InputRoleSnapshot, SensingCandidateDraft},
};

const INBOX_SECRET_ID: &str = "research-inbox";
const MAX_SEEN_MESSAGE_IDS: usize = 400;
const MAX_SEEN_REMOTE_IDS: usize = 2_000;
const MAX_MAIL_BODY_CHARS: usize = 24_000;
const MAX_POLL_MESSAGES: usize = 24;
const POLL_TIMEOUT: Duration = Duration::from_secs(25);
const NORMALIZATION_VERSION: u32 = 3;
const IMAP_CLIENT_IDENTITY: [(&str, Option<&str>); 3] = [
    ("name", Some("symbiont-d")),
    ("version", Some(env!("CARGO_PKG_VERSION"))),
    ("vendor", Some("symbiont-d")),
];

/// The one private mailbox that accepts reports from any number of outside
/// services. Its sender allow-list is mandatory when enabled so an arbitrary
/// e-mail cannot become model context.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailInputConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default = "default_folder")]
    pub folder: String,
    #[serde(default)]
    pub credential_store: CredentialStore,
    #[serde(skip_serializing, default)]
    pub credential_value: Option<String>,
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
}

impl Default for MailInputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: default_name(),
            host: String::new(),
            port: default_port(),
            username: String::new(),
            folder: default_folder(),
            credential_store: CredentialStore::ConfigFile,
            credential_value: None,
            allowed_senders: Vec::new(),
            max_messages: default_max_messages(),
        }
    }
}

fn default_name() -> String {
    "Research Inbox".to_owned()
}
fn default_port() -> u16 {
    993
}
fn default_folder() -> String {
    "INBOX".to_owned()
}
fn default_max_messages() -> usize {
    12
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailInputRuntime {
    #[serde(default)]
    normalization_version: u32,
    #[serde(default)]
    seen_message_ids: VecDeque<String>,
    #[serde(default)]
    seen_remote_ids: VecDeque<String>,
    last_started_at: Option<String>,
    last_succeeded_at: Option<String>,
    last_failed_at: Option<String>,
    last_error: Option<String>,
    last_received_at: Option<String>,
    #[serde(default)]
    last_received_count: usize,
    #[serde(default)]
    last_message_count: u32,
    #[serde(default)]
    last_searchable_message_count: usize,
    #[serde(default)]
    last_selected_message_count: usize,
    #[serde(default)]
    last_fetched_message_count: usize,
    #[serde(default)]
    last_body_message_count: usize,
    #[serde(default)]
    last_parsed_message_count: usize,
    #[serde(default)]
    last_allowed_message_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailInputSnapshot {
    #[serde(flatten)]
    pub config: MailInputConfig,
    pub active_credential_store: CredentialStore,
    pub credential_status: String,
    pub debug_credential_override: bool,
    pub availability: String,
    pub last_started_at: Option<String>,
    pub last_succeeded_at: Option<String>,
    pub last_failed_at: Option<String>,
    pub last_error: Option<String>,
    pub last_received_at: Option<String>,
    pub last_received_count: usize,
    pub last_message_count: u32,
    pub last_searchable_message_count: usize,
    pub last_selected_message_count: usize,
    pub last_fetched_message_count: usize,
    pub last_body_message_count: usize,
    pub last_parsed_message_count: usize,
    pub last_allowed_message_count: usize,
}

/// Result of an end-to-end, read-only mailbox health check. It exercises the
/// same search, fetch and MIME parsing path as background intake, but does not
/// persist a cursor, create candidates, or alter remote message flags.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailInputConnectionTest {
    pub checked_at: String,
    pub folder: String,
    pub message_count: u32,
    pub searchable_message_count: usize,
    pub selected_message_count: usize,
    pub fetched_message_count: usize,
    pub body_message_count: usize,
    pub parsed_message_count: usize,
    pub allowed_message_count: usize,
    pub candidate_count: usize,
}

pub struct MailInputOutcome {
    pub candidates: Vec<SensingCandidateDraft>,
    pub actor: Option<InputRoleSnapshot>,
    pub interrupted: bool,
    pub inbox_failure: Option<String>,
}

pub struct MailInputStore {
    config_path: PathBuf,
    runtime_path: PathBuf,
    config: RwLock<MailInputConfig>,
    runtime: RwLock<MailInputRuntime>,
    credentials: SecretStore,
    test_cancellation: Mutex<Option<oneshot::Sender<()>>>,
}

impl MailInputStore {
    pub async fn open(config_path: PathBuf) -> Result<Self> {
        let config = match fs::read_to_string(&config_path).await {
            Ok(value) => match toml::from_str::<MailInputConfig>(&value) {
                Ok(config) if validate_config(&config).is_ok() => config,
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %config_path.display(), "mail input configuration is invalid; using defaults");
                    MailInputConfig::default()
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => MailInputConfig::default(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read mail input configuration {}", config_path.display())
                });
            }
        };
        let runtime_path = config_path.with_extension("runtime.json");
        let credential_path = config_path.with_file_name("mail-input-secrets.toml");
        let mut runtime = read_runtime(&runtime_path).await?;
        if migrate_runtime(&mut runtime) {
            // The previous whole-message representation could permanently
            // consume a mixed digest after one weak item poisoned review.
            // Replay the bounded mailbox window once under the atomic-topic
            // normalizer instead of silently losing those independent inputs.
            persist_runtime(&runtime_path, &runtime).await?;
            tracing::info!(
                version = NORMALIZATION_VERSION,
                "mail input cursor reset for candidate normalization upgrade"
            );
        }
        persist_config(&config_path, &config).await?;
        Ok(Self {
            config_path,
            runtime_path,
            config: RwLock::new(config),
            runtime: RwLock::new(runtime),
            credentials: SecretStore::open(credential_path).await?,
            test_cancellation: Mutex::new(None),
        })
    }

    pub async fn snapshot(&self) -> MailInputSnapshot {
        let config = self.config.read().await.clone();
        let runtime = self.runtime.read().await.clone();
        let credential_status = self
            .credentials
            .status(INBOX_SECRET_ID, config.credential_store)
            .await;
        MailInputSnapshot {
            availability: availability(&config, credential_status),
            active_credential_store: self.credentials.active_store(config.credential_store),
            credential_status: credential_status.as_str().to_owned(),
            debug_credential_override: self.credentials.debug_override(config.credential_store),
            config,
            last_started_at: runtime.last_started_at,
            last_succeeded_at: runtime.last_succeeded_at,
            last_failed_at: runtime.last_failed_at,
            last_error: runtime.last_error,
            last_received_at: runtime.last_received_at,
            last_received_count: runtime.last_received_count,
            last_message_count: runtime.last_message_count,
            last_searchable_message_count: runtime.last_searchable_message_count,
            last_selected_message_count: runtime.last_selected_message_count,
            last_fetched_message_count: runtime.last_fetched_message_count,
            last_body_message_count: runtime.last_body_message_count,
            last_parsed_message_count: runtime.last_parsed_message_count,
            last_allowed_message_count: runtime.last_allowed_message_count,
        }
    }

    pub async fn update(&self, mut config: MailInputConfig) -> Result<MailInputSnapshot> {
        validate_config(&config)?;
        if let Some(secret) = config.credential_value.as_deref() {
            self.credentials
                .write(INBOX_SECRET_ID, config.credential_store, secret)
                .await?;
        }
        config.credential_value = None;
        persist_config(&self.config_path, &config).await?;
        *self.config.write().await = config;
        Ok(self.snapshot().await)
    }

    pub async fn has_configured_input(&self) -> bool {
        let config = self.config.read().await.clone();
        config.enabled
            && self
                .credentials
                .status(INBOX_SECRET_ID, config.credential_store)
                .await
                == CredentialStatus::Configured
    }

    /// Verifies an in-memory mailbox form snapshot through the full intake
    /// read path.
    ///
    /// This intentionally stays separate from [`Self::poll`]: a settings
    /// check may fetch bodies with `BODY.PEEK`, but never alters the cursor,
    /// creates candidates, changes remote flags, or spends an exploration run.
    pub async fn test_connection(
        &self,
        mut config: MailInputConfig,
    ) -> Result<MailInputConnectionTest> {
        validate_connection_config(&config)?;
        let secret = match config.credential_value.take() {
            Some(secret) if !secret.trim().is_empty() => secret,
            _ => self
                .credentials
                .read(INBOX_SECRET_ID, config.credential_store)
                .await?
                .context("research inbox credential is not configured")?,
        };
        let (cancellation_tx, mut cancellation_rx) = oneshot::channel();
        {
            let mut active_test = self.test_cancellation.lock().await;
            if active_test.is_some() {
                anyhow::bail!("research inbox connection test is already running");
            }
            *active_test = Some(cancellation_tx);
        }
        let no_seen_remote_ids = HashSet::new();
        let result = tokio::select! {
            result = timeout(POLL_TIMEOUT, read_mailbox(&config, secret, &no_seen_remote_ids)) => {
                match result {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::Error::new(error)
                        .context("test research inbox connection timed out")),
                }
            }
            _ = &mut cancellation_rx => Err(anyhow::anyhow!("research inbox connection test cancelled")),
        };
        *self.test_cancellation.lock().await = None;
        let mailbox = result?;
        let allowed_messages = mailbox
            .messages
            .iter()
            .filter(|message| sender_allowed(&message.document.sender, &config.allowed_senders))
            .collect::<Vec<_>>();
        Ok(MailInputConnectionTest {
            checked_at: timestamp(Utc::now()),
            folder: config.folder.trim().to_owned(),
            message_count: mailbox.message_count,
            searchable_message_count: mailbox.searchable_message_count,
            selected_message_count: mailbox.selected_message_count,
            fetched_message_count: mailbox.fetched_message_count,
            body_message_count: mailbox.body_message_count,
            parsed_message_count: mailbox.parsed_message_count,
            allowed_message_count: allowed_messages.len(),
            candidate_count: allowed_messages
                .into_iter()
                .map(|message| message.document.candidate_count())
                .sum::<usize>()
                .min(3),
        })
    }

    /// Stops the currently running settings-only connection test, if any.
    /// It never affects the normal background mailbox poll.
    pub async fn cancel_connection_test(&self) -> bool {
        self.test_cancellation
            .lock()
            .await
            .take()
            .is_some_and(|sender| sender.send(()).is_ok())
    }

    /// Polls the selected mailbox without changing any message flags or
    /// mailbox state. New user input cancels the network operation promptly.
    pub async fn poll(&self, mut input_events: watch::Receiver<u64>) -> Result<MailInputOutcome> {
        let config = self.config.read().await.clone();
        if !config.enabled {
            return Ok(empty_outcome());
        }
        let secret = match self
            .credentials
            .read(INBOX_SECRET_ID, config.credential_store)
            .await?
        {
            Some(secret) => secret,
            None => return Ok(empty_outcome()),
        };
        self.update_runtime(|runtime| runtime.last_started_at = Some(timestamp(Utc::now())))
            .await?;
        let seen_remote_ids = self
            .runtime
            .read()
            .await
            .seen_remote_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let result = tokio::select! {
            result = timeout(POLL_TIMEOUT, read_mailbox(&config, secret, &seen_remote_ids)) => match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::Error::new(error).context("poll research inbox timed out")),
            },
            changed = input_events.changed() => {
                changed.context("watch newer user input during mailbox polling")?;
                return Ok(MailInputOutcome { interrupted: true, ..empty_outcome() });
            }
        };
        match result {
            Ok(mailbox) => {
                let allowed_message_count = mailbox
                    .messages
                    .iter()
                    .filter(|message| {
                        sender_allowed(&message.document.sender, &config.allowed_senders)
                    })
                    .count();
                let candidates = self.accept_new_messages(&config, mailbox.messages).await?;
                self.update_runtime(|runtime| {
                    runtime.last_succeeded_at = Some(timestamp(Utc::now()));
                    runtime.last_error = None;
                    runtime.last_received_count = candidates.len();
                    runtime.last_message_count = mailbox.message_count;
                    runtime.last_searchable_message_count = mailbox.searchable_message_count;
                    runtime.last_selected_message_count = mailbox.selected_message_count;
                    runtime.last_fetched_message_count = mailbox.fetched_message_count;
                    runtime.last_body_message_count = mailbox.body_message_count;
                    runtime.last_parsed_message_count = mailbox.parsed_message_count;
                    runtime.last_allowed_message_count = allowed_message_count;
                    if !candidates.is_empty() {
                        runtime.last_received_at = Some(timestamp(Utc::now()));
                    }
                })
                .await?;
                Ok(MailInputOutcome {
                    candidates,
                    actor: Some(InputRoleSnapshot::mailbox(&config.name)),
                    interrupted: false,
                    inbox_failure: None,
                })
            }
            Err(error) => {
                let message = compact_error(&error.to_string());
                self.update_runtime(|runtime| {
                    runtime.last_failed_at = Some(timestamp(Utc::now()));
                    runtime.last_error = Some(message.clone());
                })
                .await?;
                tracing::warn!(%error, "research inbox failed without fallback");
                Ok(MailInputOutcome {
                    inbox_failure: Some(message),
                    ..empty_outcome()
                })
            }
        }
    }

    async fn accept_new_messages(
        &self,
        config: &MailInputConfig,
        messages: Vec<RawMail>,
    ) -> Result<Vec<SensingCandidateDraft>> {
        let mut runtime = self.runtime.write().await;
        let candidates = select_new_messages(&mut runtime, config, messages);
        let snapshot = runtime.clone();
        drop(runtime);
        persist_runtime(&self.runtime_path, &snapshot).await?;
        Ok(candidates)
    }

    async fn update_runtime(&self, update: impl FnOnce(&mut MailInputRuntime)) -> Result<()> {
        let mut runtime = self.runtime.write().await;
        update(&mut runtime);
        let snapshot = runtime.clone();
        drop(runtime);
        persist_runtime(&self.runtime_path, &snapshot).await
    }
}

fn select_new_messages(
    runtime: &mut MailInputRuntime,
    config: &MailInputConfig,
    messages: Vec<RawMail>,
) -> Vec<SensingCandidateDraft> {
    let mut candidates = Vec::new();
    for message in messages {
        if runtime
            .seen_remote_ids
            .iter()
            .any(|seen| seen == &message.remote_id)
        {
            continue;
        }
        remember_with_limit(
            &mut runtime.seen_remote_ids,
            message.remote_id.clone(),
            MAX_SEEN_REMOTE_IDS,
        );
        if runtime
            .seen_message_ids
            .iter()
            .any(|seen| seen == &message.id)
        {
            continue;
        }
        remember_with_limit(
            &mut runtime.seen_message_ids,
            message.id.clone(),
            MAX_SEEN_MESSAGE_IDS,
        );
        if !sender_allowed(&message.document.sender, &config.allowed_senders)
            || candidates.len() >= 3
        {
            continue;
        }
        for candidate in message.document.into_candidates() {
            if candidates.len() >= 3 {
                break;
            }
            candidates.push(candidate);
        }
    }
    candidates
}

fn empty_outcome() -> MailInputOutcome {
    MailInputOutcome {
        candidates: Vec::new(),
        actor: None,
        interrupted: false,
        inbox_failure: None,
    }
}

#[derive(Debug)]
struct RawMail {
    remote_id: String,
    id: String,
    document: MailDocument,
}

struct MailboxRead {
    message_count: u32,
    searchable_message_count: usize,
    selected_message_count: usize,
    fetched_message_count: usize,
    body_message_count: usize,
    parsed_message_count: usize,
    messages: Vec<RawMail>,
}

async fn read_mailbox(
    config: &MailInputConfig,
    secret: String,
    seen_remote_ids: &HashSet<String>,
) -> Result<MailboxRead> {
    let mut inbox = connect_mailbox(config, &secret).await?;
    let mailbox = inbox
        .examine(config.folder.trim())
        .await
        .map_err(|error| anyhow::anyhow!("open IMAP folder read-only: {error:?}"))?;
    let mut uids = inbox
        .uid_search("ALL")
        .await
        .context("list IMAP message UIDs")?
        .drain()
        .collect::<Vec<_>>();
    uids.sort_unstable();
    let searchable_message_count = uids.len();
    let uid_validity = mailbox.uid_validity.unwrap_or_default();
    let limit = config.max_messages.clamp(1, MAX_POLL_MESSAGES);
    let uids = uids
        .into_iter()
        .rev()
        .filter(|uid| !seen_remote_ids.contains(&remote_message_id(config, uid_validity, *uid)))
        .take(limit)
        .collect::<Vec<_>>();
    let selected_message_count = uids.len();
    if uids.is_empty() {
        let _ = inbox.logout().await;
        return Ok(MailboxRead {
            message_count: mailbox.exists,
            searchable_message_count,
            selected_message_count,
            fetched_message_count: 0,
            body_message_count: 0,
            parsed_message_count: 0,
            messages: Vec::new(),
        });
    }
    let uid_set = uids
        .iter()
        .rev()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let (mut messages, fetched_message_count, body_message_count) = {
        let mut fetched = inbox
            .uid_fetch(&uid_set, "(UID BODY.PEEK[])")
            .await
            .context("fetch IMAP messages without marking read")?;
        let mut messages = Vec::new();
        let mut fetched_message_count = 0;
        let mut body_message_count = 0;
        while let Some(message) = fetched.next().await {
            let message = message.context("read IMAP message")?;
            fetched_message_count += 1;
            let Some(body) = message
                .body()
                .map(<[u8]>::to_vec)
                .or_else(|| assemble_message(message.header(), message.text()))
            else {
                continue;
            };
            body_message_count += 1;
            let Some(uid) = message.uid else {
                continue;
            };
            if let Some(mail) = parse_message(body, remote_message_id(config, uid_validity, uid)) {
                messages.push((uid, mail));
            }
        }
        (messages, fetched_message_count, body_message_count)
    };
    let _ = inbox.logout().await;
    messages.sort_unstable_by_key(|(uid, _)| std::cmp::Reverse(*uid));
    let messages = messages
        .into_iter()
        .map(|(_, message)| message)
        .collect::<Vec<_>>();
    let parsed_message_count = messages.len();
    Ok(MailboxRead {
        message_count: mailbox.exists,
        searchable_message_count,
        selected_message_count,
        fetched_message_count,
        body_message_count,
        parsed_message_count,
        messages,
    })
}

fn assemble_message(header: Option<&[u8]>, text: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut raw = header?.to_vec();
    if !raw.ends_with(b"\r\n\r\n") {
        if !raw.ends_with(b"\r\n") {
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(text?);
    Some(raw)
}

async fn connect_mailbox(
    config: &MailInputConfig,
    secret: &str,
) -> Result<ImapSession<TlsStream<TcpStream>>> {
    let host = config.host.trim();
    let stream = TcpStream::connect((host, config.port))
        .await
        .context("connect to IMAP over TLS")?;
    let server_name = ServerName::try_from(host)
        .context("validate IMAP TLS hostname")?
        .to_owned();
    let tls = imap_tls_connector()?
        .connect(server_name, stream)
        .await
        .context("complete IMAP TLS handshake")?;
    let mut session = ImapClient::new(tls)
        .login(config.username.trim(), secret)
        .await
        .map_err(|(error, _)| anyhow::anyhow!("authenticate IMAP account: {error:?}"))?;
    session
        .id(IMAP_CLIENT_IDENTITY)
        .await
        .map_err(|error| anyhow::anyhow!("identify IMAP client: {error:?}"))?;
    Ok(session)
}

fn imap_tls_connector() -> Result<TlsConnector> {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("configure IMAP TLS versions")?
    .with_root_certificates(root_store)
    .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

fn parse_message(raw: Vec<u8>, remote_id: String) -> Option<RawMail> {
    let parsed = MessageParser::default().parse(&raw)?;
    let sender = parsed
        .from()?
        .first()?
        .address()?
        .trim()
        .to_ascii_lowercase();
    if sender.is_empty() {
        return None;
    }
    let subject = compact_text(parsed.subject().unwrap_or("External research input"), 240);
    let body = parsed
        .body_text(0)
        .or_else(|| parsed.body_preview(MAX_MAIL_BODY_CHARS))?;
    let id = parsed
        .message_id()
        .map(str::to_owned)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| fingerprint(&raw));
    let event_at = parsed
        .date()
        .and_then(|date| DateTime::<Utc>::from_timestamp(date.to_timestamp(), 0))
        .map(timestamp);
    let document = MailDocument::new(sender, subject, body.as_ref(), event_at)?;
    Some(RawMail {
        remote_id,
        id,
        document,
    })
}

fn remote_message_id(config: &MailInputConfig, uid_validity: u32, uid: u32) -> String {
    format!(
        "{}|{}|{}|{uid_validity}|{uid}",
        config.host.trim().to_ascii_lowercase(),
        config.username.trim().to_ascii_lowercase(),
        config.folder.trim().to_ascii_lowercase(),
    )
}

fn sender_allowed(sender: &str, allowed_senders: &[String]) -> bool {
    allowed_senders
        .iter()
        .map(|allowed| allowed.trim().to_ascii_lowercase())
        .any(|allowed| {
            !allowed.is_empty()
                && (sender == allowed || (allowed.starts_with('@') && sender.ends_with(&allowed)))
        })
}

fn remember_with_limit(seen: &mut VecDeque<String>, id: String, limit: usize) {
    seen.push_back(id);
    while seen.len() > limit {
        seen.pop_front();
    }
}

fn migrate_runtime(runtime: &mut MailInputRuntime) -> bool {
    if runtime.normalization_version >= NORMALIZATION_VERSION {
        return false;
    }
    runtime.seen_message_ids.clear();
    runtime.seen_remote_ids.clear();
    runtime.normalization_version = NORMALIZATION_VERSION;
    true
}

fn fingerprint(value: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(value);
    let digest = hash.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    format!("mail:{encoded}")
}

fn compact_text(value: &str, limit: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn compact_error(value: &str) -> String {
    value.chars().take(600).collect()
}
fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn validate_config(config: &MailInputConfig) -> Result<()> {
    if config.name.trim().is_empty() || config.name.chars().count() > 120 {
        anyhow::bail!("mail input name must contain at most 120 characters");
    }
    if config.enabled {
        if config.host.trim().is_empty() || config.host.chars().count() > 250 {
            anyhow::bail!("mail input host is required and must contain at most 250 characters");
        }
        if config.username.trim().is_empty() || config.username.chars().count() > 320 {
            anyhow::bail!(
                "mail input username is required and must contain at most 320 characters"
            );
        }
        if config.folder.trim().is_empty() || config.folder.chars().count() > 320 {
            anyhow::bail!("mail input folder is required and must contain at most 320 characters");
        }
        if config.allowed_senders.is_empty()
            || config
                .allowed_senders
                .iter()
                .any(|sender| sender.trim().is_empty() || sender.chars().count() > 320)
        {
            anyhow::bail!("mail input requires one or more allowed sender addresses or domains");
        }
    }
    if config.max_messages == 0 || config.max_messages > MAX_POLL_MESSAGES {
        anyhow::bail!("mail input max messages must be between 1 and {MAX_POLL_MESSAGES}");
    }
    Ok(())
}

fn validate_connection_config(config: &MailInputConfig) -> Result<()> {
    if config.host.trim().is_empty() || config.host.chars().count() > 250 {
        anyhow::bail!("mail input host is required and must contain at most 250 characters");
    }
    if config.port == 0 {
        anyhow::bail!("mail input port is required");
    }
    if config.username.trim().is_empty() || config.username.chars().count() > 320 {
        anyhow::bail!("mail input username is required and must contain at most 320 characters");
    }
    if config.folder.trim().is_empty() || config.folder.chars().count() > 320 {
        anyhow::bail!("mail input folder is required and must contain at most 320 characters");
    }
    Ok(())
}

fn availability(config: &MailInputConfig, credential_status: CredentialStatus) -> String {
    if !config.enabled {
        return "disabled".to_owned();
    }
    if config.host.trim().is_empty()
        || config.username.trim().is_empty()
        || config.allowed_senders.is_empty()
    {
        return "incomplete".to_owned();
    }
    match credential_status {
        CredentialStatus::Configured => "ready".to_owned(),
        CredentialStatus::Missing => "missing_credential".to_owned(),
        CredentialStatus::Unavailable => "credential_unavailable".to_owned(),
    }
}

async fn read_runtime(path: &PathBuf) -> Result<MailInputRuntime> {
    match fs::read_to_string(path).await {
        Ok(value) => serde_json::from_str(&value).context("decode mail input runtime"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(MailInputRuntime::default()),
        Err(error) => {
            Err(error).with_context(|| format!("read mail input runtime {}", path.display()))
        }
    }
}

async fn persist_config(path: &PathBuf, config: &MailInputConfig) -> Result<()> {
    persist_file(
        path,
        toml::to_string_pretty(config).context("encode mail input configuration")?,
    )
    .await
}

async fn persist_runtime(path: &PathBuf, runtime: &MailInputRuntime) -> Result<()> {
    persist_file(
        path,
        serde_json::to_string_pretty(runtime).context("encode mail input runtime")?,
    )
    .await
}

async fn persist_file(path: &PathBuf, content: String) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create mail input directory {}", parent.display()))?;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("state");
    let temporary = path.with_extension(format!("{extension}.tmp"));
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write mail input file {}", path.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace mail input file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_input_requires_an_explicit_sender_allow_list_when_enabled() {
        let config = MailInputConfig {
            enabled: true,
            host: "imap.example.com".to_owned(),
            username: "bridge@example.com".to_owned(),
            ..MailInputConfig::default()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn parses_plain_text_mail_without_preserving_raw_headers() {
        let raw = b"From: Spark <spark@google.com>\r\nSubject: Daily research\r\nMessage-ID: <daily-1@example.com>\r\nDate: Sat, 9 Aug 2026 08:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nA new paper is relevant: https://example.com/paper\r\n".to_vec();
        let message = parse_message(raw, "mailbox|1|7".to_owned()).unwrap();
        assert_eq!(message.remote_id, "mailbox|1|7");
        assert_eq!(message.document.sender, "spark@google.com");
        assert_eq!(message.id, "daily-1@example.com");
        let candidate = message.document.into_candidates().remove(0);
        assert_eq!(candidate.sources[0].url, "https://example.com/paper");
        assert!(candidate.proposed_input.contains("new paper"));
    }

    #[test]
    fn parses_html_only_mail_through_the_preview_fallback() {
        let raw = b"From: Gemini <glenzli92@gmail.com>\r\nSubject: Daily exploration\r\nMessage-ID: <gemini-1@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><h1>Useful signal</h1><p>A concrete research result.</p></body></html>\r\n".to_vec();
        let message = parse_message(raw, "mailbox|1|8".to_owned()).unwrap();
        assert!(message.document.body.contains("Useful signal"));
        assert!(message.document.body.contains("concrete research result"));
    }

    #[test]
    fn rebuilds_a_message_from_read_only_header_and_text_sections() {
        let raw = assemble_message(
            Some(b"From: Spark <spark@google.com>\r\nSubject: Daily research\r\n"),
            Some(b"A useful result."),
        )
        .unwrap();
        let message = parse_message(raw, "mailbox|1|9".to_owned()).unwrap();
        assert_eq!(message.document.sender, "spark@google.com");
        assert_eq!(message.document.body, "A useful result.");
    }

    #[test]
    fn remote_cursor_is_scoped_to_the_mailbox_and_uid_validity() {
        let config = MailInputConfig {
            host: "IMAP.EXAMPLE.COM".to_owned(),
            username: "Bridge@Example.com".to_owned(),
            folder: "INBOX".to_owned(),
            ..MailInputConfig::default()
        };
        assert_eq!(
            remote_message_id(&config, 41, 9),
            "imap.example.com|bridge@example.com|inbox|41|9"
        );
    }

    #[test]
    fn local_remote_cursor_prevents_duplicate_candidates() {
        let config = MailInputConfig {
            allowed_senders: vec!["spark@google.com".to_owned()],
            ..MailInputConfig::default()
        };
        let message = || RawMail {
            remote_id: "mailbox|1|10".to_owned(),
            id: "daily-10@example.com".to_owned(),
            document: MailDocument::new(
                "spark@google.com".to_owned(),
                "Daily research".to_owned(),
                "A useful result.",
                None,
            )
            .unwrap(),
        };
        let mut runtime = MailInputRuntime::default();
        assert_eq!(
            select_new_messages(&mut runtime, &config, vec![message()]).len(),
            1
        );
        assert!(select_new_messages(&mut runtime, &config, vec![message()]).is_empty());
        assert_eq!(runtime.seen_remote_ids.len(), 1);
        assert_eq!(runtime.seen_message_ids.len(), 1);
    }

    #[test]
    fn normalization_upgrade_replays_the_bounded_mailbox_window_once() {
        let mut runtime = MailInputRuntime {
            seen_message_ids: VecDeque::from(["old-message".to_owned()]),
            seen_remote_ids: VecDeque::from(["old-remote".to_owned()]),
            ..MailInputRuntime::default()
        };
        assert!(migrate_runtime(&mut runtime));
        assert!(runtime.seen_message_ids.is_empty());
        assert!(runtime.seen_remote_ids.is_empty());
        assert_eq!(runtime.normalization_version, NORMALIZATION_VERSION);
        assert!(!migrate_runtime(&mut runtime));
    }

    #[test]
    fn sender_allow_list_accepts_exact_addresses_or_explicit_domains() {
        assert!(sender_allowed(
            "spark@google.com",
            &["spark@google.com".to_owned()]
        ));
        assert!(sender_allowed(
            "alerts@arxiv.org",
            &["@arxiv.org".to_owned()]
        ));
        assert!(!sender_allowed(
            "other@google.com",
            &["spark@google.com".to_owned()]
        ));
    }

    #[test]
    fn connection_test_accepts_an_unsaved_connection_without_routing_rules() {
        let config = MailInputConfig {
            enabled: false,
            host: "imap.example.com".to_owned(),
            username: "bridge@example.com".to_owned(),
            ..MailInputConfig::default()
        };
        assert!(validate_connection_config(&config).is_ok());
    }

    #[test]
    fn connection_test_requires_the_mailbox_connection_fields() {
        let config = MailInputConfig {
            host: "imap.example.com".to_owned(),
            ..MailInputConfig::default()
        };
        assert!(validate_connection_config(&config).is_err());
    }

    #[test]
    fn identifies_the_imap_client_before_opening_a_mailbox() {
        assert_eq!(IMAP_CLIENT_IDENTITY[0], ("name", Some("symbiont-d")));
        assert_eq!(IMAP_CLIENT_IDENTITY[2], ("vendor", Some("symbiont-d")));
        assert!(IMAP_CLIENT_IDENTITY[1].1.is_some());
    }
}
