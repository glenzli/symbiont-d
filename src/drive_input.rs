//! Private, read-only Google Drive Inbox for externally generated documents.
//!
//! This owner contains Drive authentication, folder/file selection, remote
//! cursors and failure policy. Fetched documents are handed to the shared
//! digest normalizer and never become durable memory directly.

use std::{
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{Mutex, RwLock, oneshot, watch},
    time::timeout,
};

mod oauth;

use oauth::DriveOAuth;
pub use oauth::{
    DriveOAuthSnapshot, DriveOAuthStart, DriveOAuthStartResponse, DriveOAuthStoreSelection,
};

use crate::{
    external_digest::{DigestProvenance, ExternalDigest},
    secrets::{CredentialStore, SecretStore},
    sensing::{InputRoleSnapshot, SensingCandidateDraft},
};

const DRIVE_FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const DEFAULT_NAME: &str = "Google Drive Inbox";
const LEGACY_DEFAULT_NAME: &str = "Gemini Daily Digests";
const GOOGLE_DOCUMENT_MIME: &str = "application/vnd.google-apps.document";
const MAX_SEEN_FILE_IDS: usize = 2_000;
const MAX_LISTED_FILES: usize = 1_000;
const MAX_POLL_FILES: usize = 24;
const MAX_FILE_BYTES: usize = 1_048_576;
const POLL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriveFileSelection {
    All,
    #[default]
    Pattern,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveInputConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_folder_id")]
    pub folder_id: String,
    #[serde(default)]
    pub file_selection: DriveFileSelection,
    #[serde(default = "default_file_name_pattern")]
    pub file_name_pattern: String,
    #[serde(default)]
    pub credential_store: CredentialStore,
    #[serde(skip_serializing, default)]
    pub credential_value: Option<String>,
    #[serde(default = "default_max_files")]
    pub max_files: usize,
}

impl Default for DriveInputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: default_name(),
            folder_id: default_folder_id(),
            file_selection: DriveFileSelection::Pattern,
            file_name_pattern: default_file_name_pattern(),
            credential_store: CredentialStore::ConfigFile,
            credential_value: None,
            max_files: default_max_files(),
        }
    }
}

fn default_name() -> String {
    DEFAULT_NAME.to_owned()
}

fn default_folder_id() -> String {
    String::new()
}

fn default_file_name_pattern() -> String {
    "Digest_*.md".to_owned()
}

fn default_max_files() -> usize {
    12
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriveInputRuntime {
    #[serde(default)]
    seen_file_ids: VecDeque<String>,
    last_started_at: Option<String>,
    last_succeeded_at: Option<String>,
    last_failed_at: Option<String>,
    last_error: Option<String>,
    last_received_at: Option<String>,
    #[serde(default)]
    last_received_count: usize,
    #[serde(default)]
    last_listed_file_count: usize,
    #[serde(default)]
    last_matching_file_count: usize,
    #[serde(default)]
    last_selected_file_count: usize,
    #[serde(default)]
    last_fetched_file_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveInputSnapshot {
    #[serde(flatten)]
    pub config: DriveInputConfig,
    pub active_credential_store: CredentialStore,
    pub credential_status: String,
    pub debug_credential_override: bool,
    pub oauth: DriveOAuthSnapshot,
    pub availability: String,
    pub last_started_at: Option<String>,
    pub last_succeeded_at: Option<String>,
    pub last_failed_at: Option<String>,
    pub last_error: Option<String>,
    pub last_received_at: Option<String>,
    pub last_received_count: usize,
    pub last_listed_file_count: usize,
    pub last_matching_file_count: usize,
    pub last_selected_file_count: usize,
    pub last_fetched_file_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveInputConnectionTest {
    pub checked_at: String,
    pub folder_id: String,
    pub listed_file_count: usize,
    pub matching_file_count: usize,
    pub selected_file_count: usize,
    pub fetched_file_count: usize,
    pub candidate_count: usize,
}

pub struct DriveFileBatch {
    pub file_id: String,
    pub candidates: Vec<SensingCandidateDraft>,
}

pub struct DriveInputOutcome {
    pub batches: Vec<DriveFileBatch>,
    pub actor: Option<InputRoleSnapshot>,
    pub interrupted: bool,
    pub channel_failure: Option<String>,
}

pub struct DriveInputStore {
    config_path: PathBuf,
    runtime_path: PathBuf,
    config: RwLock<DriveInputConfig>,
    runtime: RwLock<DriveInputRuntime>,
    oauth: DriveOAuth,
    test_cancellation: Mutex<Option<oneshot::Sender<()>>>,
}

impl DriveInputStore {
    pub async fn open(config_path: PathBuf) -> Result<Self> {
        let mut config = match fs::read_to_string(&config_path).await {
            Ok(value) => match toml::from_str::<DriveInputConfig>(&value) {
                Ok(config) if validate_config(&config).is_ok() => config,
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %config_path.display(), "Drive input configuration is invalid; using defaults");
                    DriveInputConfig::default()
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => DriveInputConfig::default(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read Drive input configuration {}", config_path.display())
                });
            }
        };
        migrate_legacy_defaults(&mut config);
        let runtime_path = config_path.with_extension("runtime.json");
        let credential_path = config_path.with_file_name("drive-input-secrets.toml");
        let runtime = read_runtime(&runtime_path).await?;
        persist_config(&config_path, &config).await?;
        let credentials = Arc::new(SecretStore::open(credential_path).await?);
        let oauth = DriveOAuth::new(Arc::clone(&credentials))?;
        Ok(Self {
            config_path,
            runtime_path,
            config: RwLock::new(config),
            runtime: RwLock::new(runtime),
            oauth,
            test_cancellation: Mutex::new(None),
        })
    }

    pub async fn snapshot(&self) -> DriveInputSnapshot {
        let store = self.config.read().await.credential_store;
        self.snapshot_for_store(store).await
    }

    async fn snapshot_for_store(&self, store: CredentialStore) -> DriveInputSnapshot {
        let mut config = self.config.read().await.clone();
        config.credential_store = store;
        let runtime = self.runtime.read().await.clone();
        let oauth = self.oauth.snapshot(config.credential_store).await;
        let authorized = self.oauth.is_authorized(config.credential_store).await;
        let credential_status = if authorized {
            "configured"
        } else if oauth.status == "invalid" {
            "unavailable"
        } else {
            "missing"
        };
        DriveInputSnapshot {
            availability: availability(&config, authorized),
            active_credential_store: self.oauth.active_store(config.credential_store),
            credential_status: credential_status.to_owned(),
            debug_credential_override: self.oauth.debug_override(config.credential_store),
            oauth,
            config,
            last_started_at: runtime.last_started_at,
            last_succeeded_at: runtime.last_succeeded_at,
            last_failed_at: runtime.last_failed_at,
            last_error: runtime.last_error,
            last_received_at: runtime.last_received_at,
            last_received_count: runtime.last_received_count,
            last_listed_file_count: runtime.last_listed_file_count,
            last_matching_file_count: runtime.last_matching_file_count,
            last_selected_file_count: runtime.last_selected_file_count,
            last_fetched_file_count: runtime.last_fetched_file_count,
        }
    }

    pub async fn update(&self, mut config: DriveInputConfig) -> Result<DriveInputSnapshot> {
        validate_config(&config)?;
        anyhow::ensure!(
            config
                .credential_value
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()),
            "OAuth client JSON is used only by ‘Connect Google Drive’; connect the account before saving"
        );
        config.credential_value = None;
        persist_config(&self.config_path, &config).await?;
        *self.config.write().await = config;
        Ok(self.snapshot().await)
    }

    pub async fn has_configured_input(&self) -> bool {
        let config = self.config.read().await.clone();
        config.enabled
            && validate_enabled_fields(&config).is_ok()
            && self.oauth.is_authorized(config.credential_store).await
    }

    pub async fn start_oauth(&self, request: DriveOAuthStart) -> Result<DriveOAuthStartResponse> {
        self.oauth.start(request).await
    }

    pub async fn oauth_status(&self, selection: DriveOAuthStoreSelection) -> DriveInputSnapshot {
        self.snapshot_for_store(selection.credential_store).await
    }

    pub async fn cancel_oauth(&self) -> bool {
        self.oauth.cancel().await
    }

    pub async fn disconnect_oauth(&self, selection: DriveOAuthStoreSelection) -> Result<()> {
        self.oauth.disconnect(selection.credential_store).await
    }

    pub async fn test_connection(
        &self,
        mut config: DriveInputConfig,
    ) -> Result<DriveInputConnectionTest> {
        validate_connection_config(&config)?;
        config.credential_value = None;
        let access_token = self
            .oauth
            .access_token(config.credential_store)
            .await
            .context("authorize the personal Google Drive account")?;
        let (cancellation_tx, mut cancellation_rx) = oneshot::channel();
        {
            let mut active_test = self.test_cancellation.lock().await;
            if active_test.is_some() {
                anyhow::bail!("Google Drive connection test is already running");
            }
            *active_test = Some(cancellation_tx);
        }
        let no_seen_files = HashSet::new();
        let result = tokio::select! {
            result = timeout(POLL_TIMEOUT, read_drive(&config, &access_token, &no_seen_files, config.max_files)) => {
                match result {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::Error::new(error).context("test Google Drive input timed out")),
                }
            }
            _ = &mut cancellation_rx => Err(anyhow::anyhow!("Google Drive connection test cancelled")),
        };
        *self.test_cancellation.lock().await = None;
        let drive = result?;
        Ok(DriveInputConnectionTest {
            checked_at: timestamp(Utc::now()),
            folder_id: config.folder_id.trim().to_owned(),
            listed_file_count: drive.listed_file_count,
            matching_file_count: drive.matching_file_count,
            selected_file_count: drive.selected_file_count,
            fetched_file_count: drive.fetched_file_count,
            candidate_count: drive
                .batches
                .iter()
                .map(|batch| batch.candidates.len())
                .sum(),
        })
    }

    pub async fn cancel_connection_test(&self) -> bool {
        self.test_cancellation
            .lock()
            .await
            .take()
            .is_some_and(|sender| sender.send(()).is_ok())
    }

    pub async fn poll(
        &self,
        mut input_events: watch::Receiver<u64>,
        candidate_capacity: usize,
    ) -> Result<DriveInputOutcome> {
        let config = self.config.read().await.clone();
        if !config.enabled {
            return Ok(empty_outcome());
        }
        self.update_runtime(|runtime| runtime.last_started_at = Some(timestamp(Utc::now())))
            .await?;
        let access_token = match self.oauth.access_token(config.credential_store).await {
            Ok(token) => token,
            Err(_) if !self.oauth.is_authorized(config.credential_store).await => {
                return Ok(empty_outcome());
            }
            Err(error) => {
                return self
                    .failed_poll(error.context("authorize the personal Google Drive account"))
                    .await;
            }
        };
        let seen_file_ids = self
            .runtime
            .read()
            .await
            .seen_file_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let maximum_files = config.max_files.min(candidate_capacity);
        let result = tokio::select! {
            result = timeout(POLL_TIMEOUT, read_drive(&config, &access_token, &seen_file_ids, maximum_files)) => {
                match result {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::Error::new(error).context("poll Google Drive input timed out")),
                }
            }
            changed = input_events.changed() => {
                changed.context("watch newer user input during Google Drive polling")?;
                return Ok(DriveInputOutcome { interrupted: true, ..empty_outcome() });
            }
        };
        match result {
            Ok(drive) => {
                self.update_runtime(|runtime| {
                    runtime.last_succeeded_at = Some(timestamp(Utc::now()));
                    runtime.last_error = None;
                    runtime.last_listed_file_count = drive.listed_file_count;
                    runtime.last_matching_file_count = drive.matching_file_count;
                    runtime.last_selected_file_count = drive.selected_file_count;
                    runtime.last_fetched_file_count = drive.fetched_file_count;
                })
                .await?;
                Ok(DriveInputOutcome {
                    batches: drive.batches,
                    actor: Some(InputRoleSnapshot::drive(&config.name)),
                    interrupted: false,
                    channel_failure: None,
                })
            }
            Err(error) => self.failed_poll(error).await,
        }
    }

    async fn failed_poll(&self, error: anyhow::Error) -> Result<DriveInputOutcome> {
        let message = compact_error(&format!("{error:#}"));
        self.update_runtime(|runtime| {
            runtime.last_failed_at = Some(timestamp(Utc::now()));
            runtime.last_error = Some(message.clone());
        })
        .await?;
        tracing::warn!(%error, "Google Drive Inbox input failed");
        Ok(DriveInputOutcome {
            channel_failure: Some(message),
            ..empty_outcome()
        })
    }

    pub async fn acknowledge_files(&self, file_ids: Vec<String>) -> Result<()> {
        if file_ids.is_empty() {
            return Ok(());
        }
        let file_count = file_ids.len();
        self.update_runtime(|runtime| {
            for file_id in file_ids {
                if !runtime.seen_file_ids.iter().any(|seen| seen == &file_id) {
                    remember_with_limit(&mut runtime.seen_file_ids, file_id, MAX_SEEN_FILE_IDS);
                }
            }
            runtime.last_received_at = Some(timestamp(Utc::now()));
            runtime.last_received_count = file_count;
        })
        .await
    }

    async fn update_runtime(&self, update: impl FnOnce(&mut DriveInputRuntime)) -> Result<()> {
        let mut runtime = self.runtime.write().await;
        update(&mut runtime);
        let snapshot = runtime.clone();
        drop(runtime);
        persist_runtime(&self.runtime_path, &snapshot).await
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFileList {
    #[serde(default)]
    files: Vec<DriveFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: String,
    name: String,
    mime_type: String,
    #[serde(default)]
    web_view_link: Option<String>,
}

struct DriveRead {
    listed_file_count: usize,
    matching_file_count: usize,
    selected_file_count: usize,
    fetched_file_count: usize,
    batches: Vec<DriveFileBatch>,
}

async fn read_drive(
    config: &DriveInputConfig,
    access_token: &str,
    seen_file_ids: &HashSet<String>,
    maximum_files: usize,
) -> Result<DriveRead> {
    let client = Client::builder()
        .timeout(POLL_TIMEOUT)
        .build()
        .context("create Google Drive HTTP client")?;
    let query = format!(
        "'{}' in parents and trashed = false and mimeType != 'application/vnd.google-apps.folder'",
        config.folder_id.trim()
    );
    let page_size = MAX_LISTED_FILES.to_string();
    let response = client
        .get(DRIVE_FILES_URL)
        .bearer_auth(access_token)
        .query(&[
            ("q", query.as_str()),
            ("orderBy", "createdTime desc"),
            ("pageSize", page_size.as_str()),
            ("supportsAllDrives", "true"),
            ("includeItemsFromAllDrives", "true"),
            ("fields", "files(id,name,mimeType,webViewLink)"),
        ])
        .send()
        .await
        .context("list files in Google Drive Inbox folder")?;
    let files: DriveFileList = response_json(response, "list Google Drive Inbox files").await?;
    let listed_file_count = files.files.len();
    let matching_files = files
        .files
        .into_iter()
        .filter(supported_drive_file)
        .filter(|file| file_selected(config, &file.name))
        .collect::<Vec<_>>();
    let matching_file_count = matching_files.len();
    let mut selected_files = matching_files
        .into_iter()
        .filter(|file| !seen_file_ids.contains(&file.id))
        .take(maximum_files.min(MAX_POLL_FILES))
        .collect::<Vec<_>>();
    let selected_file_count = selected_files.len();
    selected_files.reverse();
    let mut batches = Vec::with_capacity(selected_files.len());
    for file in selected_files {
        let body = download_drive_file(&client, access_token, &file).await?;
        let fallback_url = file
            .web_view_link
            .clone()
            .unwrap_or_else(|| format!("https://drive.google.com/open?id={}", file.id));
        let candidates = ExternalDigest::new(
            file.name.clone(),
            &body,
            // The Drive file timestamp is when the digest document was
            // created, not when every event described by its sections
            // happened. Candidate observation time is recorded when the file
            // enters the local queue; assigning this timestamp as `event_at`
            // made identical recurring sections look like new events.
            None,
            DigestProvenance {
                fallback_url,
                source_detail: format!(
                    "Linked through the user-configured Google Drive Inbox from {}",
                    file.name
                ),
                possible_connection: "User-configured private Google Drive Inbox; standalone interest is allowed and no project connection is claimed".to_owned(),
            },
        )
        .map(ExternalDigest::into_candidates)
        .unwrap_or_default();
        batches.push(DriveFileBatch {
            file_id: file.id,
            candidates,
        });
    }
    let fetched_file_count = batches.len();
    Ok(DriveRead {
        listed_file_count,
        matching_file_count,
        selected_file_count,
        fetched_file_count,
        batches,
    })
}

async fn download_drive_file(
    client: &Client,
    access_token: &str,
    file: &DriveFile,
) -> Result<String> {
    let response = if file.mime_type == GOOGLE_DOCUMENT_MIME {
        client
            .get(format!("{DRIVE_FILES_URL}/{}/export", file.id))
            .bearer_auth(access_token)
            .query(&[("mimeType", "text/markdown")])
            .send()
            .await
            .with_context(|| format!("export Google Drive document {}", file.name))?
    } else {
        client
            .get(format!("{DRIVE_FILES_URL}/{}", file.id))
            .bearer_auth(access_token)
            .query(&[("alt", "media")])
            .send()
            .await
            .with_context(|| format!("download Google Drive file {}", file.name))?
    };
    let bytes = response_bytes(response, "download Google Drive Inbox file").await?;
    String::from_utf8(bytes).context("Google Drive Inbox file is not valid UTF-8 text")
}

async fn response_json<T: for<'de> Deserialize<'de>>(
    response: Response,
    action: &str,
) -> Result<T> {
    let status = response.status();
    let bytes = response_bytes(response, action).await?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode response for {action} ({status})"))
}

async fn response_bytes(response: Response, action: &str) -> Result<Vec<u8>> {
    let status = response.status();
    if let Some(length) = response.content_length()
        && length > MAX_FILE_BYTES as u64
    {
        anyhow::bail!("{action} exceeded the {MAX_FILE_BYTES}-byte safety limit");
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("read response for {action}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        anyhow::bail!("{action} exceeded the {MAX_FILE_BYTES}-byte safety limit");
    }
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        anyhow::bail!("{action} failed with {status}: {}", compact_error(&detail));
    }
    Ok(bytes.to_vec())
}

fn supported_drive_file(file: &DriveFile) -> bool {
    file.mime_type == GOOGLE_DOCUMENT_MIME
        || file.mime_type.starts_with("text/")
        || file.name.to_ascii_lowercase().ends_with(".md")
        || file.name.to_ascii_lowercase().ends_with(".markdown")
        || file.name.to_ascii_lowercase().ends_with(".txt")
}

fn file_selected(config: &DriveInputConfig, name: &str) -> bool {
    match config.file_selection {
        DriveFileSelection::All => true,
        DriveFileSelection::Pattern => glob_matches(config.file_name_pattern.trim(), name),
    }
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut matched = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    matched[0][0] = true;
    for pattern_index in 1..=pattern.len() {
        if pattern[pattern_index - 1] == '*' {
            matched[pattern_index][0] = matched[pattern_index - 1][0];
        }
        for value_index in 1..=value.len() {
            matched[pattern_index][value_index] = match pattern[pattern_index - 1] {
                '*' => {
                    matched[pattern_index - 1][value_index]
                        || matched[pattern_index][value_index - 1]
                }
                '?' => matched[pattern_index - 1][value_index - 1],
                literal => {
                    literal == value[value_index - 1] && matched[pattern_index - 1][value_index - 1]
                }
            };
        }
    }
    matched[pattern.len()][value.len()]
}

fn validate_config(config: &DriveInputConfig) -> Result<()> {
    if config.name.trim().is_empty() || config.name.chars().count() > 120 {
        anyhow::bail!("Drive input name must contain at most 120 characters");
    }
    if config.enabled {
        validate_enabled_fields(config)?;
    } else {
        validate_pattern(config)?;
    }
    if config.max_files == 0 || config.max_files > MAX_POLL_FILES {
        anyhow::bail!("Drive input max files must be between 1 and {MAX_POLL_FILES}");
    }
    Ok(())
}

fn migrate_legacy_defaults(config: &mut DriveInputConfig) {
    if config.name == LEGACY_DEFAULT_NAME {
        config.name = default_name();
    }
}

fn validate_connection_config(config: &DriveInputConfig) -> Result<()> {
    validate_folder_id(&config.folder_id)?;
    validate_pattern(config)?;
    if config.max_files == 0 || config.max_files > MAX_POLL_FILES {
        anyhow::bail!("Drive input max files must be between 1 and {MAX_POLL_FILES}");
    }
    Ok(())
}

fn validate_enabled_fields(config: &DriveInputConfig) -> Result<()> {
    validate_folder_id(&config.folder_id)?;
    validate_pattern(config)
}

fn validate_folder_id(folder_id: &str) -> Result<()> {
    let folder_id = folder_id.trim();
    if folder_id.is_empty()
        || folder_id.len() > 200
        || !folder_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        anyhow::bail!("Drive folder ID is required and must be a valid Google Drive resource ID");
    }
    Ok(())
}

fn validate_pattern(config: &DriveInputConfig) -> Result<()> {
    if config.file_selection == DriveFileSelection::Pattern {
        let pattern = config.file_name_pattern.trim();
        if pattern.is_empty()
            || pattern.chars().count() > 240
            || pattern
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        {
            anyhow::bail!(
                "Drive filename pattern is required and must be a filename-only glob of at most 240 characters"
            );
        }
    }
    Ok(())
}

fn availability(config: &DriveInputConfig, authorized: bool) -> String {
    if !config.enabled {
        return "disabled".to_owned();
    }
    if validate_enabled_fields(config).is_err() {
        return "incomplete".to_owned();
    }
    if authorized {
        "ready".to_owned()
    } else {
        "missing_credential".to_owned()
    }
}

fn empty_outcome() -> DriveInputOutcome {
    DriveInputOutcome {
        batches: Vec::new(),
        actor: None,
        interrupted: false,
        channel_failure: None,
    }
}

fn remember_with_limit(seen: &mut VecDeque<String>, id: String, limit: usize) {
    seen.push_back(id);
    while seen.len() > limit {
        seen.pop_front();
    }
}

fn compact_error(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(800)
        .collect()
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn read_runtime(path: &PathBuf) -> Result<DriveInputRuntime> {
    match fs::read_to_string(path).await {
        Ok(value) => serde_json::from_str(&value).context("decode Drive input runtime"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(DriveInputRuntime::default()),
        Err(error) => {
            Err(error).with_context(|| format!("read Drive input runtime {}", path.display()))
        }
    }
}

async fn persist_config(path: &PathBuf, config: &DriveInputConfig) -> Result<()> {
    persist_file(
        path,
        toml::to_string_pretty(config).context("encode Drive input configuration")?,
    )
    .await
}

async fn persist_runtime(path: &PathBuf, runtime: &DriveInputRuntime) -> Result<()> {
    persist_file(
        path,
        serde_json::to_string_pretty(runtime).context("encode Drive input runtime")?,
    )
    .await
}

async fn persist_file(path: &PathBuf, content: String) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create Drive input directory {}", parent.display()))?;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("state");
    let temporary = path.with_extension(format!("{extension}.tmp"));
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write Drive input file {}", path.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace Drive input file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, mime_type: &str) -> DriveFile {
        DriveFile {
            id: format!("id-{name}"),
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            web_view_link: None,
        }
    }

    #[test]
    fn filename_glob_supports_all_literal_wildcard_and_single_character_matches() {
        assert!(glob_matches("Digest_*.md", "Digest_20260810_1200.md"));
        assert!(glob_matches(
            "Digest_2026081?_1200.md",
            "Digest_20260810_1200.md"
        ));
        assert!(!glob_matches("Digest_*.md", "Notes_20260810.md"));
        assert!(!glob_matches("Digest_?.md", "Digest_12.md"));
    }

    #[test]
    fn selection_can_read_every_supported_file_or_apply_a_pattern() {
        let mut config = DriveInputConfig::default();
        let digest = file("Digest_20260810_1200.md", "text/markdown");
        let notes = file("Notes.md", "text/markdown");
        assert!(file_selected(&config, &digest.name));
        assert!(!file_selected(&config, &notes.name));
        config.file_selection = DriveFileSelection::All;
        assert!(file_selected(&config, &notes.name));
    }

    #[test]
    fn markdown_blobs_and_google_documents_are_supported() {
        assert!(supported_drive_file(&file(
            "Digest.md",
            "application/octet-stream"
        )));
        assert!(supported_drive_file(&file("Digest", GOOGLE_DOCUMENT_MIME)));
        assert!(!supported_drive_file(&file(
            "Digest.pdf",
            "application/pdf"
        )));
    }

    #[test]
    fn enabled_input_requires_a_folder_and_pattern_when_selected() {
        let mut config = DriveInputConfig {
            enabled: true,
            ..DriveInputConfig::default()
        };
        config.folder_id.clear();
        assert!(validate_config(&config).is_err());
        config.folder_id = "example-folder-id".to_owned();
        assert!(validate_config(&config).is_ok());
        config.file_name_pattern.clear();
        assert!(validate_config(&config).is_err());
        config.file_selection = DriveFileSelection::All;
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn acknowledgement_is_idempotent_and_bounded() {
        let mut seen = VecDeque::new();
        for index in 0..MAX_SEEN_FILE_IDS + 2 {
            remember_with_limit(&mut seen, format!("file-{index}"), MAX_SEEN_FILE_IDS);
        }
        assert_eq!(seen.len(), MAX_SEEN_FILE_IDS);
        assert_eq!(seen.front().map(String::as_str), Some("file-2"));
    }

    #[test]
    fn api_page_size_stays_within_the_drive_limit() {
        assert_eq!(MAX_LISTED_FILES, 1_000);
        assert!(DriveInputConfig::default().folder_id.is_empty());
        assert_eq!(DriveInputConfig::default().name, DEFAULT_NAME);
    }

    #[test]
    fn legacy_digest_channel_name_migrates_without_overwriting_custom_names() {
        let mut legacy = DriveInputConfig {
            name: LEGACY_DEFAULT_NAME.to_owned(),
            ..DriveInputConfig::default()
        };
        migrate_legacy_defaults(&mut legacy);
        assert_eq!(legacy.name, DEFAULT_NAME);

        legacy.name = "Research Documents".to_owned();
        migrate_legacy_defaults(&mut legacy);
        assert_eq!(legacy.name, "Research Documents");
    }

    #[test]
    fn drive_status_errors_keep_a_bounded_server_detail() {
        assert_eq!(reqwest::StatusCode::FORBIDDEN.as_u16(), 403);
        assert_eq!(compact_error(" permission\n denied "), "permission denied");
    }
}
