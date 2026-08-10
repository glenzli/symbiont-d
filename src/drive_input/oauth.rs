//! Personal Google Drive authorization for the local Drive input.
//!
//! This owner contains the desktop OAuth lifecycle: PKCE state, the temporary
//! loopback callback, refresh-token persistence and access-token refresh. The
//! Drive reader only receives a short-lived bearer token and never handles
//! Google account credentials itself.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, RwLock, oneshot},
    time::timeout,
};

use crate::secrets::{CredentialStore, SecretStore};

const DRIVE_SECRET_ID: &str = "google-drive-digests";
const DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_DRIVE_ABOUT_URL: &str = "https://www.googleapis.com/drive/v3/about";
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const CREDENTIAL_SCHEMA: &str = "symbiont.google-drive.oauth";
const CREDENTIAL_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
struct OAuthClientFile {
    installed: Option<OAuthClientDocument>,
}

#[derive(Clone, Debug, Deserialize)]
struct OAuthClientDocument {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Clone, Debug)]
struct OAuthClient {
    client_id: String,
    client_secret: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AuthorizedCredential {
    schema: String,
    version: u32,
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
}

impl AuthorizedCredential {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema == CREDENTIAL_SCHEMA && self.version == CREDENTIAL_VERSION,
            "stored Google Drive authorization uses an unsupported format"
        );
        validate_client_id(&self.client_id)?;
        anyhow::ensure!(
            !self.refresh_token.trim().is_empty(),
            "stored Google Drive authorization has no refresh token"
        );
        Ok(())
    }

    fn client(&self) -> OAuthClient {
        OAuthClient {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveAbout {
    #[serde(default)]
    user: Option<DriveUser>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveUser {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    email_address: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveOAuthSnapshot {
    pub status: String,
    pub account: Option<String>,
    pub expires_at: Option<String>,
    pub error: Option<String>,
}

impl DriveOAuthSnapshot {
    fn disconnected() -> Self {
        Self {
            status: "disconnected".to_owned(),
            account: None,
            expires_at: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveOAuthStart {
    #[serde(default)]
    pub credential_store: CredentialStore,
    #[serde(default)]
    pub credential_value: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveOAuthStartResponse {
    pub authorization_url: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveOAuthStoreSelection {
    #[serde(default)]
    pub credential_store: CredentialStore,
}

#[derive(Clone)]
struct PendingFlow {
    id: String,
    store: CredentialStore,
    snapshot: DriveOAuthSnapshot,
}

#[derive(Default)]
struct FlowState {
    pending: Option<PendingFlow>,
    terminal: Option<(CredentialStore, DriveOAuthSnapshot)>,
}

struct CachedAccessToken {
    credential_fingerprint: String,
    token: String,
    expires_at: DateTime<Utc>,
}

pub struct DriveOAuth {
    credentials: Arc<SecretStore>,
    client: Client,
    flow: Arc<RwLock<FlowState>>,
    cancellation: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    token_cache: Arc<Mutex<Option<CachedAccessToken>>>,
}

impl DriveOAuth {
    pub fn new(credentials: Arc<SecretStore>) -> Result<Self> {
        let client = Client::builder()
            .timeout(OAUTH_HTTP_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .context("create Google OAuth HTTP client")?;
        Ok(Self {
            credentials,
            client,
            flow: Arc::new(RwLock::new(FlowState::default())),
            cancellation: Arc::new(Mutex::new(None)),
            token_cache: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn snapshot(&self, store: CredentialStore) -> DriveOAuthSnapshot {
        let state = self.flow.read().await;
        if let Some(pending) = state.pending.as_ref()
            && pending.store == store
        {
            return pending.snapshot.clone();
        }
        if let Some((terminal_store, snapshot)) = state.terminal.as_ref()
            && *terminal_store == store
        {
            return snapshot.clone();
        }
        drop(state);
        match self.read_credential(store).await {
            Ok(Some(credential)) => DriveOAuthSnapshot {
                status: "connected".to_owned(),
                account: credential.account,
                expires_at: None,
                error: None,
            },
            Ok(None) => DriveOAuthSnapshot::disconnected(),
            Err(error) => DriveOAuthSnapshot {
                status: "invalid".to_owned(),
                account: None,
                expires_at: None,
                error: Some(compact_error(&format!("{error:#}"))),
            },
        }
    }

    pub fn active_store(&self, requested: CredentialStore) -> CredentialStore {
        self.credentials.active_store(requested)
    }

    pub fn debug_override(&self, requested: CredentialStore) -> bool {
        self.credentials.debug_override(requested)
    }

    pub async fn is_authorized(&self, store: CredentialStore) -> bool {
        self.read_credential(store)
            .await
            .is_ok_and(|credential| credential.is_some())
    }

    pub async fn start(&self, request: DriveOAuthStart) -> Result<DriveOAuthStartResponse> {
        if self.flow.read().await.pending.is_some() {
            anyhow::bail!("Google Drive authorization is already waiting for the browser");
        }
        let client_config = match request
            .credential_value
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(value) => parse_client_file(value)?,
            None => self
                .read_credential(request.credential_store)
                .await?
                .context("Choose a Google desktop OAuth client JSON first")?
                .client(),
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("open the local Google OAuth callback")?;
        let callback_address = listener
            .local_addr()
            .context("read the local Google OAuth callback address")?;
        let redirect_uri = format!("http://127.0.0.1:{}", callback_address.port());
        let flow_id = random_token(32)?;
        let state = random_token(32)?;
        let verifier = random_token(64)?;
        let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
        let expires_at = Utc::now()
            + chrono::Duration::from_std(OAUTH_CALLBACK_TIMEOUT)
                .context("represent Google authorization timeout")?;
        let authorization_url =
            authorization_url(&client_config, &redirect_uri, &state, &challenge)?;
        let (cancellation_tx, cancellation_rx) = oneshot::channel();
        *self.cancellation.lock().await = Some(cancellation_tx);
        self.flow.write().await.pending = Some(PendingFlow {
            id: flow_id.clone(),
            store: request.credential_store,
            snapshot: DriveOAuthSnapshot {
                status: "waiting".to_owned(),
                account: None,
                expires_at: Some(timestamp(expires_at)),
                error: None,
            },
        });

        let task = AuthorizationTask {
            listener,
            client_config,
            redirect_uri,
            expected_state: state,
            verifier,
            store: request.credential_store,
        };
        let credentials = Arc::clone(&self.credentials);
        let client = self.client.clone();
        let flow = Arc::clone(&self.flow);
        let cancellation = Arc::clone(&self.cancellation);
        let token_cache = Arc::clone(&self.token_cache);
        tokio::spawn(async move {
            let result = run_authorization(task, credentials, client, cancellation_rx).await;
            let snapshot = match &result {
                Ok(authorized) => DriveOAuthSnapshot {
                    status: "connected".to_owned(),
                    account: authorized.account.clone(),
                    expires_at: None,
                    error: None,
                },
                Err(error) => DriveOAuthSnapshot {
                    status: "failed".to_owned(),
                    account: None,
                    expires_at: None,
                    error: Some(compact_error(&format!("{error:#}"))),
                },
            };
            let mut state = flow.write().await;
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == flow_id)
            {
                let store = state.pending.as_ref().map(|pending| pending.store).unwrap();
                state.pending = None;
                state.terminal = Some((store, snapshot));
                if let Ok(authorized) = result {
                    *token_cache.lock().await = Some(authorized.access_token);
                }
                cancellation.lock().await.take();
            }
        });
        Ok(DriveOAuthStartResponse {
            authorization_url,
            expires_at: timestamp(expires_at),
        })
    }

    pub async fn cancel(&self) -> bool {
        let cancelled = self
            .cancellation
            .lock()
            .await
            .take()
            .is_some_and(|sender| sender.send(()).is_ok());
        if cancelled {
            let mut state = self.flow.write().await;
            state.pending = None;
            state.terminal = None;
        }
        cancelled
    }

    pub async fn disconnect(&self, store: CredentialStore) -> Result<()> {
        self.cancel().await;
        self.credentials.remove(DRIVE_SECRET_ID, store).await?;
        *self.token_cache.lock().await = None;
        self.flow.write().await.terminal = None;
        Ok(())
    }

    pub async fn access_token(&self, store: CredentialStore) -> Result<String> {
        let credential = self
            .read_credential(store)
            .await?
            .context("Connect a personal Google Drive account first")?;
        let fingerprint = credential_fingerprint(&credential)?;
        if let Some(cached) = self.token_cache.lock().await.as_ref()
            && cached.credential_fingerprint == fingerprint
            && cached.expires_at > Utc::now() + chrono::Duration::seconds(60)
        {
            return Ok(cached.token.clone());
        }
        let token = refresh_access_token(&self.client, &credential).await?;
        let value = token.token.clone();
        *self.token_cache.lock().await = Some(token);
        Ok(value)
    }

    async fn read_credential(
        &self,
        store: CredentialStore,
    ) -> Result<Option<AuthorizedCredential>> {
        let Some(raw) = self.credentials.read(DRIVE_SECRET_ID, store).await? else {
            return Ok(None);
        };
        let credential = serde_json::from_str::<AuthorizedCredential>(&raw)
            .context("decode stored personal Google Drive authorization")?;
        credential.validate()?;
        Ok(Some(credential))
    }
}

struct AuthorizationTask {
    listener: TcpListener,
    client_config: OAuthClient,
    redirect_uri: String,
    expected_state: String,
    verifier: String,
    store: CredentialStore,
}

struct AuthorizedFlow {
    account: Option<String>,
    access_token: CachedAccessToken,
}

async fn run_authorization(
    task: AuthorizationTask,
    credentials: Arc<SecretStore>,
    client: Client,
    mut cancellation: oneshot::Receiver<()>,
) -> Result<AuthorizedFlow> {
    let (mut stream, _) = tokio::select! {
        accepted = timeout(OAUTH_CALLBACK_TIMEOUT, task.listener.accept()) => {
            accepted.context("Google authorization timed out")??
        }
        _ = &mut cancellation => anyhow::bail!("Google authorization was cancelled"),
    };
    let result = async {
        let callback = read_callback(&mut stream).await?;
        let completed = complete_authorization(&client, &task, callback).await?;
        let serialized = serde_json::to_string(&completed.credential)
            .context("encode personal Google Drive authorization")?;
        credentials
            .write(DRIVE_SECRET_ID, task.store, &serialized)
            .await?;
        Ok::<_, anyhow::Error>(AuthorizedFlow {
            account: completed.credential.account,
            access_token: completed.access_token,
        })
    }
    .await;
    let response = match &result {
        Ok(_) => callback_page(
            "Google Drive 已连接",
            "授权已经安全保存，可以关闭这个页面并返回 Symbiont。",
        ),
        Err(error) => callback_page(
            "Google Drive 连接失败",
            &compact_error(&format!("{error:#}")),
        ),
    };
    let _ = stream.write_all(response.as_bytes()).await;
    result
}

struct CallbackParameters {
    code: String,
    state: String,
}

struct CompletedAuthorization {
    credential: AuthorizedCredential,
    access_token: CachedAccessToken,
}

async fn complete_authorization(
    client: &Client,
    task: &AuthorizationTask,
    callback: CallbackParameters,
) -> Result<CompletedAuthorization> {
    let callback = parse_callback(callback, &task.expected_state)?;
    let mut parameters = vec![
        ("client_id", task.client_config.client_id.as_str()),
        ("code", callback.code.as_str()),
        ("code_verifier", task.verifier.as_str()),
        ("grant_type", "authorization_code"),
        ("redirect_uri", task.redirect_uri.as_str()),
    ];
    if let Some(secret) = task.client_config.client_secret.as_deref() {
        parameters.push(("client_secret", secret));
    }
    let response = post_form(client, GOOGLE_TOKEN_URL, &parameters)
        .await
        .context("exchange the Google authorization code")?;
    let refresh_token = response
        .refresh_token
        .clone()
        .context("Google did not return an offline refresh token; reconnect and grant access")?;
    anyhow::ensure!(
        !response.access_token.trim().is_empty(),
        "Google authorization response omitted the access token"
    );
    let account = fetch_account(client, &response.access_token)
        .await
        .ok()
        .flatten();
    let credential = AuthorizedCredential {
        schema: CREDENTIAL_SCHEMA.to_owned(),
        version: CREDENTIAL_VERSION,
        client_id: task.client_config.client_id.clone(),
        client_secret: task.client_config.client_secret.clone(),
        refresh_token,
        account,
    };
    let fingerprint = credential_fingerprint(&credential)?;
    Ok(CompletedAuthorization {
        credential,
        access_token: cache_token(fingerprint, response),
    })
}

async fn refresh_access_token(
    client: &Client,
    credential: &AuthorizedCredential,
) -> Result<CachedAccessToken> {
    let mut parameters = vec![
        ("client_id", credential.client_id.as_str()),
        ("refresh_token", credential.refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    if let Some(secret) = credential.client_secret.as_deref() {
        parameters.push(("client_secret", secret));
    }
    let response = post_form(client, GOOGLE_TOKEN_URL, &parameters)
        .await
        .context("refresh personal Google Drive authorization")?;
    anyhow::ensure!(
        !response.access_token.trim().is_empty(),
        "Google refresh response omitted the access token"
    );
    Ok(cache_token(credential_fingerprint(credential)?, response))
}

fn cache_token(fingerprint: String, response: TokenResponse) -> CachedAccessToken {
    let lifetime = response.expires_in.unwrap_or(3_600).clamp(60, 86_400);
    CachedAccessToken {
        credential_fingerprint: fingerprint,
        token: response.access_token,
        expires_at: Utc::now() + chrono::Duration::seconds(lifetime),
    }
}

async fn post_form(
    client: &Client,
    endpoint: &str,
    fields: &[(&str, &str)],
) -> Result<TokenResponse> {
    let encoded = form_body(fields)?;
    let response = client
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(encoded)
        .send()
        .await
        .context("send request to Google OAuth")?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .context("read Google OAuth response")?;
    if !status.is_success() {
        anyhow::bail!(
            "Google OAuth returned {status}: {}",
            oauth_error_detail(&bytes)
        );
    }
    serde_json::from_slice(&bytes).context("decode Google OAuth token response")
}

async fn fetch_account(client: &Client, access_token: &str) -> Result<Option<String>> {
    let response = client
        .get(GOOGLE_DRIVE_ABOUT_URL)
        .bearer_auth(access_token)
        .query(&[("fields", "user(displayName,emailAddress)")])
        .send()
        .await
        .context("read the authorized Google Drive account")?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .context("read Google Drive account response")?;
    anyhow::ensure!(
        status.is_success(),
        "Google Drive account lookup returned {status}"
    );
    let about: DriveAbout =
        serde_json::from_slice(&bytes).context("decode Google Drive account response")?;
    Ok(about.user.and_then(|user| {
        user.email_address
            .or(user.display_name)
            .filter(|value| !value.trim().is_empty())
    }))
}

async fn read_callback(stream: &mut TcpStream) -> Result<CallbackParameters> {
    let mut bytes = Vec::with_capacity(2_048);
    loop {
        if bytes.len() >= MAX_CALLBACK_BYTES {
            anyhow::bail!("Google OAuth callback exceeded the local request limit");
        }
        let mut buffer = [0_u8; 1_024];
        let read = stream
            .read(&mut buffer)
            .await
            .context("read the local Google OAuth callback")?;
        anyhow::ensure!(
            read > 0,
            "Google OAuth callback closed before sending a request"
        );
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes).context("Google OAuth callback was not UTF-8")?;
    let request_line = request
        .lines()
        .next()
        .context("Google OAuth callback was empty")?;
    let mut parts = request_line.split_whitespace();
    anyhow::ensure!(
        parts.next() == Some("GET"),
        "Google OAuth callback must use GET"
    );
    let target = parts
        .next()
        .context("Google OAuth callback omitted its target")?;
    anyhow::ensure!(
        parts
            .next()
            .is_some_and(|version| version.starts_with("HTTP/1.")),
        "Google OAuth callback used an unsupported HTTP version"
    );
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .context("parse Google OAuth callback parameters")?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        anyhow::bail!("Google authorization was not granted ({error})");
    }
    Ok(CallbackParameters {
        code: code.context("Google OAuth callback omitted code")?,
        state: state.context("Google OAuth callback omitted state")?,
    })
}

fn parse_callback(
    callback: CallbackParameters,
    expected_state: &str,
) -> Result<CallbackParameters> {
    anyhow::ensure!(
        callback.state == expected_state,
        "Google OAuth callback state did not match"
    );
    Ok(callback)
}

fn parse_client_file(value: &str) -> Result<OAuthClient> {
    let file: OAuthClientFile =
        serde_json::from_str(value).context("decode Google desktop OAuth client JSON")?;
    let installed = file.installed.context(
        "This is not a Google Desktop app OAuth client JSON; create an OAuth client with application type Desktop app",
    )?;
    validate_client_id(&installed.client_id)?;
    let client_secret = installed
        .client_secret
        .filter(|secret| !secret.trim().is_empty());
    Ok(OAuthClient {
        client_id: installed.client_id,
        client_secret,
    })
}

fn validate_client_id(value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value == value.trim()
            && value.ends_with(".apps.googleusercontent.com")
            && value.len() <= 512,
        "Google desktop OAuth client_id is invalid"
    );
    Ok(())
}

fn authorization_url(
    client: &OAuthClient,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<String> {
    let mut url = Url::parse(GOOGLE_AUTH_URL).context("parse Google authorization endpoint")?;
    url.query_pairs_mut()
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", DRIVE_READONLY_SCOPE)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

fn form_body(fields: &[(&str, &str)]) -> Result<String> {
    let mut url = Url::parse("https://local.invalid/").context("create OAuth form encoder")?;
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in fields {
            query.append_pair(key, value);
        }
    }
    Ok(url.query().unwrap_or_default().to_owned())
}

fn random_token(byte_count: usize) -> Result<String> {
    let mut bytes = vec![0_u8; byte_count];
    getrandom::fill(&mut bytes).context("obtain operating-system randomness for Google OAuth")?;
    Ok(base64_url(&bytes))
}

fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(third & 0x3f) as usize] as char);
        }
    }
    encoded
}

fn credential_fingerprint(credential: &AuthorizedCredential) -> Result<String> {
    let bytes =
        serde_json::to_vec(credential).context("encode Google authorization fingerprint")?;
    Ok(base64_url(&Sha256::digest(bytes)))
}

fn callback_page(title: &str, message: &str) -> String {
    let title = html_escape(title);
    let message = html_escape(message);
    let body = format!(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{title}</title><style>body{{font-family:-apple-system,BlinkMacSystemFont,sans-serif;max-width:36rem;margin:12vh auto;padding:2rem;color:#242424}}p{{color:#666;line-height:1.6}}</style><h1>{title}</h1><p>{message}</p>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn oauth_error_detail(bytes: &[u8]) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        error: String,
        #[serde(default)]
        error_description: String,
    }
    serde_json::from_slice::<ErrorBody>(bytes)
        .ok()
        .map(|body| compact_error(&format!("{} {}", body.error, body.error_description)))
        .filter(|detail| !detail.is_empty())
        .unwrap_or_else(|| "Google rejected the authorization request".to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_a_desktop_oauth_client_document() {
        let client = parse_client_file(
            r#"{"installed":{"client_id":"123.apps.googleusercontent.com","client_secret":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(client.client_id, "123.apps.googleusercontent.com");
        assert_eq!(client.client_secret.as_deref(), Some("secret"));

        assert!(
            parse_client_file(r#"{"web":{"client_id":"123.apps.googleusercontent.com"}}"#).is_err()
        );
        assert!(
            parse_client_file(r#"{"type":"service_account","client_email":"x@example.com"}"#)
                .is_err()
        );
    }

    #[test]
    fn authorization_url_is_pkce_loopback_offline_and_read_only() {
        let url = authorization_url(
            &OAuthClient {
                client_id: "123.apps.googleusercontent.com".to_owned(),
                client_secret: None,
            },
            "http://127.0.0.1:43123",
            "state-token",
            "challenge-token",
        )
        .unwrap();
        let url = Url::parse(&url).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(url.as_str().split('?').next(), Some(GOOGLE_AUTH_URL));
        assert_eq!(
            query.get("redirect_uri").map(|value| value.as_ref()),
            Some("http://127.0.0.1:43123")
        );
        assert_eq!(
            query.get("scope").map(|value| value.as_ref()),
            Some(DRIVE_READONLY_SCOPE)
        );
        assert_eq!(
            query.get("access_type").map(|value| value.as_ref()),
            Some("offline")
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
    }

    #[test]
    fn base64_url_is_unpadded_and_uses_url_safe_symbols() {
        assert_eq!(base64_url(&[0xfb, 0xff, 0xef]), "-__v");
        assert_eq!(base64_url(&[0xff]), "_w");
        assert!(!base64_url(&[1, 2]).contains('='));
    }

    #[test]
    fn callback_state_must_match() {
        let callback = CallbackParameters {
            code: "authorization-code".to_owned(),
            state: "expected".to_owned(),
        };
        assert_eq!(
            parse_callback(callback, "expected").unwrap().code,
            "authorization-code"
        );
        let callback = CallbackParameters {
            code: "authorization-code".to_owned(),
            state: "wrong".to_owned(),
        };
        assert!(parse_callback(callback, "expected").is_err());
    }
}
