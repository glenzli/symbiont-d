//! Private, read-only IMAP intake for external research reports and alerts.
//!
//! This module owns the mailbox protocol, credentials, cursor and failure
//! policy. It deliberately emits only short-lived sensing candidates: e-mail
//! is an external input surface, never a memory-writing surface.

use std::{collections::VecDeque, io::ErrorKind, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use imap_rs::{
    client::{SearchKey, SearchQuery},
    connect_tls,
    credentials::Password,
};
use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    sync::{RwLock, watch},
    time::timeout,
};

use crate::{
    secrets::{CredentialStatus, CredentialStore, SecretStore},
    sensing::{InputRoleSnapshot, SensingCandidateDraft, SensingSource, SensingSourceClass},
};

const INBOX_SECRET_ID: &str = "research-inbox";
const MAX_SEEN_MESSAGE_IDS: usize = 400;
const MAX_BODY_CHARS: usize = 1_800;
const MAX_POLL_MESSAGES: usize = 24;
const POLL_TIMEOUT: Duration = Duration::from_secs(25);

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
    seen_message_ids: VecDeque<String>,
    last_started_at: Option<String>,
    last_succeeded_at: Option<String>,
    last_failed_at: Option<String>,
    last_error: Option<String>,
    last_received_at: Option<String>,
    #[serde(default)]
    last_received_count: usize,
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
        let runtime = read_runtime(&runtime_path).await?;
        persist_config(&config_path, &config).await?;
        Ok(Self {
            config_path,
            runtime_path,
            config: RwLock::new(config),
            runtime: RwLock::new(runtime),
            credentials: SecretStore::open(credential_path).await?,
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
        let result = tokio::select! {
            result = timeout(POLL_TIMEOUT, poll_mailbox(&config, secret)) => match result {
                Ok(result) => result,
                Err(error) => Err(anyhow::Error::new(error).context("poll research inbox timed out")),
            },
            changed = input_events.changed() => {
                changed.context("watch newer user input during mailbox polling")?;
                return Ok(MailInputOutcome { interrupted: true, ..empty_outcome() });
            }
        };
        match result {
            Ok(messages) => {
                let candidates = self.accept_new_messages(&config, messages).await?;
                self.update_runtime(|runtime| {
                    runtime.last_succeeded_at = Some(timestamp(Utc::now()));
                    runtime.last_error = None;
                    runtime.last_received_count = candidates.len();
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
        let mut candidates = Vec::new();
        for message in messages {
            if runtime
                .seen_message_ids
                .iter()
                .any(|seen| seen == &message.id)
            {
                continue;
            }
            remember(&mut runtime.seen_message_ids, message.id.clone());
            if !sender_allowed(&message.sender, &config.allowed_senders) || candidates.len() >= 3 {
                continue;
            }
            candidates.push(message.into_candidate());
        }
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
    id: String,
    sender: String,
    subject: String,
    body: String,
    event_at: Option<String>,
}

impl RawMail {
    fn into_candidate(self) -> SensingCandidateDraft {
        let sources = extract_sources(&self.body, &self.sender, &self.subject);
        SensingCandidateDraft {
            title: self.subject.clone(),
            summary: compact_text(&self.body, 1_000),
            proposed_input: compact_text(&self.body, MAX_BODY_CHARS),
            event_at: self.event_at,
            source_class: SensingSourceClass::OpenDiscovery,
            possible_connection: Some(format!("Private research inbox · {}", self.sender)),
            sources,
        }
    }
}

async fn poll_mailbox(config: &MailInputConfig, secret: String) -> Result<Vec<RawMail>> {
    let session = connect_tls(config.host.trim(), config.port)
        .await
        .context("connect to IMAP over TLS")?;
    let auth = session
        .login(config.username.trim(), Password::new(secret))
        .await
        .context("authenticate IMAP account")?;
    let mut inbox = auth
        .examine(config.folder.trim())
        .await
        .context("open IMAP folder read-only")?;
    let mut sequences = inbox
        .search(SearchQuery::new(SearchKey::All))
        .await
        .context("list IMAP messages")?;
    sequences.sort_unstable();
    let limit = config.max_messages.clamp(1, MAX_POLL_MESSAGES);
    let sequences = sequences.into_iter().rev().take(limit).collect::<Vec<_>>();
    if sequences.is_empty() {
        let _ = inbox.logout().await;
        return Ok(Vec::new());
    }
    let sequence_set = sequences
        .iter()
        .rev()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fetched = inbox
        .fetch(&sequence_set, "UID BODY.PEEK[]")
        .await
        .context("fetch IMAP messages without marking read")?;
    let _ = inbox.logout().await;
    let mut messages = fetched
        .into_iter()
        .filter_map(|message| message.body)
        .filter_map(parse_message)
        .collect::<Vec<_>>();
    messages.reverse();
    Ok(messages)
}

fn parse_message(raw: Vec<u8>) -> Option<RawMail> {
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
    let body = compact_text(parsed.body_text(0).as_deref().unwrap_or(""), MAX_BODY_CHARS);
    if body.is_empty() {
        return None;
    }
    let id = parsed
        .message_id()
        .map(str::to_owned)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| fingerprint(&raw));
    let event_at = parsed
        .date()
        .and_then(|date| DateTime::<Utc>::from_timestamp(date.to_timestamp(), 0))
        .map(timestamp);
    Some(RawMail {
        id,
        sender,
        subject,
        body,
        event_at,
    })
}

fn extract_sources(body: &str, sender: &str, subject: &str) -> Vec<SensingSource> {
    let sources = body
        .split_whitespace()
        .filter_map(|token| {
            let url = token.trim_matches(|character: char| {
                matches!(
                    character,
                    '<' | '>' | '(' | ')' | '[' | ']' | ',' | '.' | ';' | '"' | '\''
                )
            });
            (url.starts_with("https://") || url.starts_with("http://")).then(|| SensingSource {
                url: url.to_owned(),
                detail: format!("Linked from {sender}: {subject}"),
            })
        })
        .take(3)
        .collect::<Vec<_>>();
    if sources.is_empty() {
        vec![SensingSource {
            url: format!("mailto:{sender}"),
            detail: format!("Private research inbox: {subject}"),
        }]
    } else {
        sources
    }
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

fn remember(seen: &mut VecDeque<String>, id: String) {
    seen.push_back(id);
    while seen.len() > MAX_SEEN_MESSAGE_IDS {
        seen.pop_front();
    }
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
        let message = parse_message(raw).unwrap();
        assert_eq!(message.sender, "spark@google.com");
        assert_eq!(message.id, "daily-1@example.com");
        let candidate = message.into_candidate();
        assert_eq!(candidate.sources[0].url, "https://example.com/paper");
        assert!(candidate.proposed_input.contains("new paper"));
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
}
