//! Local speech-to-text boundary backed by infer-runtime.
//!
//! This owner keeps microphone payloads out of conversation persistence.  It
//! accepts one short-lived upload, forwards it only to a loopback
//! infer-runtime instance, and returns editable text to the browser.  The
//! browser owns recording and discard; PCP only sees text the user sends.

mod discovery;

use std::{env, io::ErrorKind, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::{Client, StatusCode, Url, multipart, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{fs, sync::RwLock};

use crate::secrets::{CredentialStatus, CredentialStore, SecretStore};
use discovery::{DiscoveredConsumer, canonical_loopback_origin, discover_consumer};

const TRANSCRIPTION_SECRET_ID: &str = "infer-runtime";
const ENDPOINT_OVERRIDE_ENV: &str = "SYMBIONT_INFER_RUNTIME_BASE_URL";
const COMPATIBILITY_FALLBACK: &str = "http://127.0.0.1:8787";
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub credential_store: CredentialStore,
    #[serde(skip_serializing, default)]
    pub credential_value: Option<String>,
    #[serde(default = "default_language")]
    pub language: String,
}

impl Default for AudioTranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            credential_store: CredentialStore::ConfigFile,
            credential_value: None,
            language: default_language(),
        }
    }
}

fn default_language() -> String {
    "zh".to_owned()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionSnapshot {
    #[serde(flatten)]
    pub config: AudioTranscriptionConfig,
    pub active_credential_store: CredentialStore,
    pub credential_status: String,
    pub debug_credential_override: bool,
    pub availability: String,
    pub resolved_base_url: String,
    pub endpoint_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_lease_expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionResult {
    pub id: String,
    pub text: String,
    pub model: String,
}

/// A local-only infer-runtime client plus its user-controlled configuration.
pub struct AudioTranscriptionStore {
    config_path: PathBuf,
    config: RwLock<AudioTranscriptionConfig>,
    credentials: SecretStore,
    client: Client,
    resolved_endpoint: RwLock<Option<ResolvedConsumerEndpoint>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EndpointSource {
    Environment,
    Settings,
    Discovery,
    CompatibilityFallback,
}

impl EndpointSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Settings => "settings",
            Self::Discovery => "discovery",
            Self::CompatibilityFallback => "compatibility_fallback",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedConsumerEndpoint {
    base_url: String,
    source: EndpointSource,
    instance_id: Option<String>,
    generation: Option<String>,
    lease_expires_at: Option<chrono::DateTime<Utc>>,
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

impl AudioTranscriptionStore {
    pub async fn open(config_path: PathBuf) -> Result<Self> {
        let mut config = match fs::read_to_string(&config_path).await {
            Ok(value) => match toml::from_str::<AudioTranscriptionConfig>(&value) {
                Ok(config) if validate_config(&config).is_ok() => config,
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %config_path.display(), "audio transcription configuration is invalid; using defaults");
                    AudioTranscriptionConfig::default()
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                AudioTranscriptionConfig::default()
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "read audio transcription configuration {}",
                        config_path.display()
                    )
                });
            }
        };
        if config.base_url.trim() == COMPATIBILITY_FALLBACK {
            config.base_url.clear();
        }
        persist_config(&config_path, &config).await?;
        let credential_path = config_path.with_file_name("infer-runtime-secrets.toml");
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            credentials: SecretStore::open(credential_path).await?,
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .no_proxy()
                .redirect(Policy::none())
                .build()?,
            resolved_endpoint: RwLock::new(None),
        })
    }

    pub async fn snapshot(&self) -> AudioTranscriptionSnapshot {
        let config = self.config.read().await.clone();
        let credential_status = self
            .credentials
            .status(TRANSCRIPTION_SECRET_ID, config.credential_store)
            .await;
        let endpoint = self.resolve_endpoint(&config).await;
        if let Err(error) = &endpoint {
            tracing::warn!(error = %error, "infer-runtime endpoint resolution failed");
        }
        let endpoint_ready = endpoint.is_ok();
        let (
            resolved_base_url,
            endpoint_source,
            endpoint_instance_id,
            endpoint_generation,
            endpoint_lease_expires_at,
        ) = endpoint
            .as_ref()
            .map(|endpoint| {
                (
                    endpoint.base_url.clone(),
                    endpoint.source.as_str().to_owned(),
                    endpoint.instance_id.clone(),
                    endpoint.generation.clone(),
                    endpoint
                        .lease_expires_at
                        .as_ref()
                        .map(chrono::DateTime::to_rfc3339),
                )
            })
            .unwrap_or_else(|_| (String::new(), "unavailable".to_owned(), None, None, None));
        AudioTranscriptionSnapshot {
            availability: availability(&config, credential_status, endpoint_ready),
            active_credential_store: self.credentials.active_store(config.credential_store),
            credential_status: credential_status.as_str().to_owned(),
            debug_credential_override: self.credentials.debug_override(config.credential_store),
            resolved_base_url,
            endpoint_source,
            endpoint_instance_id,
            endpoint_generation,
            endpoint_lease_expires_at,
            config,
        }
    }

    pub async fn update(
        &self,
        mut config: AudioTranscriptionConfig,
    ) -> Result<AudioTranscriptionSnapshot> {
        validate_config(&config)?;
        if let Some(secret) = config.credential_value.as_deref() {
            self.credentials
                .write(TRANSCRIPTION_SECRET_ID, config.credential_store, secret)
                .await?;
        }
        config.credential_value = None;
        persist_config(&self.config_path, &config).await?;
        *self.config.write().await = config;
        Ok(self.snapshot().await)
    }

    pub async fn transcribe(
        &self,
        filename: Option<&str>,
        mime_type: Option<&str>,
        audio: Vec<u8>,
    ) -> Result<TranscriptionResult> {
        if audio.is_empty() {
            anyhow::bail!("没有收到录音数据");
        }
        if audio.len() > MAX_AUDIO_BYTES {
            anyhow::bail!("录音超过 25 MiB，建议缩短后再试");
        }
        tracing::info!(
            target: crate::runtime_log::TARGET,
            event = "voice_transcription_started",
            bytes = audio.len(),
            "local voice transcription started"
        );
        let config = self.config.read().await.clone();
        if !config.enabled {
            anyhow::bail!("本地语音转写尚未启用");
        }
        validate_config(&config)?;
        let token = self
            .credentials
            .read(TRANSCRIPTION_SECRET_ID, config.credential_store)
            .await?
            .ok_or_else(|| anyhow::anyhow!("本地语音转写尚未配置访问令牌"))?;
        let selected = self.resolve_endpoint(&config).await?;
        let file_name = filename
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("voice-input.webm");
        let response = match self
            .send_transcription(
                &selected,
                &token,
                file_name,
                mime_type,
                &config.language,
                &audio,
            )
            .await
        {
            Ok(response) => response,
            Err(first_error) => {
                let refreshed = self.resolve_endpoint(&config).await?;
                if refreshed == selected {
                    return Err(first_error).context("contact local infer-runtime");
                }
                self.send_transcription(
                    &refreshed,
                    &token,
                    file_name,
                    mime_type,
                    &config.language,
                    &audio,
                )
                .await
                .context("contact rediscovered local infer-runtime")?
            }
        };
        let status = response.status();
        let payload = response
            .json::<Value>()
            .await
            .context("decode infer-runtime response")?;
        if !status.is_success() {
            anyhow::bail!(runtime_error(status, &payload));
        }
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("本地转写没有返回可用文本"))?
            .to_owned();
        let result = TranscriptionResult {
            id: payload
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("local-transcription")
                .to_owned(),
            model: payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("audio.transcribe")
                .to_owned(),
            text,
        };
        tracing::info!(
            target: crate::runtime_log::TARGET,
            event = "voice_transcription_completed",
            chars = result.text.chars().count(),
            "local voice transcription completed"
        );
        Ok(result)
    }

    async fn resolve_endpoint(
        &self,
        config: &AudioTranscriptionConfig,
    ) -> Result<ResolvedConsumerEndpoint> {
        let selected = resolve_endpoint(config)?;
        *self.resolved_endpoint.write().await = Some(selected.clone());
        Ok(selected)
    }

    async fn send_transcription(
        &self,
        selected: &ResolvedConsumerEndpoint,
        token: &str,
        file_name: &str,
        mime_type: Option<&str>,
        language: &str,
        audio: &[u8],
    ) -> Result<reqwest::Response> {
        let mut part = multipart::Part::bytes(audio.to_vec()).file_name(file_name.to_owned());
        if let Some(mime_type) = mime_type.filter(|value| !value.trim().is_empty()) {
            part = part
                .mime_str(mime_type)
                .context("invalid audio MIME type")?;
        }
        let metadata = json!({
            "infer.priority": "interactive",
            "infer.placement": "local_only",
            "infer.prefer": "local",
            "infer.offline_required": "true",
            "infer.fallback": "none",
        });
        let form = multipart::Form::new()
            .text("model", "audio.transcribe")
            .part("file", part)
            .text("language", language.to_owned())
            .text("response_format", "verbose_json")
            .text("metadata", metadata.to_string());
        self.client
            .post(transcription_endpoint(&selected.base_url)?)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .context("send infer-runtime transcription request")
    }
}

fn transcription_endpoint(base_url: &str) -> Result<Url> {
    let base_url = canonical_loopback_origin(base_url)?;
    Url::parse(&format!("{base_url}/v1/audio/transcriptions"))
        .context("build infer-runtime transcription endpoint")
}

fn resolve_endpoint(config: &AudioTranscriptionConfig) -> Result<ResolvedConsumerEndpoint> {
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
    if !config.base_url.trim().is_empty() {
        return Ok(ResolvedConsumerEndpoint {
            base_url: canonical_loopback_origin(&config.base_url)?,
            source: EndpointSource::Settings,
            instance_id: None,
            generation: None,
            lease_expires_at: None,
        });
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

fn validate_config(config: &AudioTranscriptionConfig) -> Result<()> {
    let language = config.language.trim();
    if language.is_empty() || language.chars().count() > 16 {
        anyhow::bail!("语音识别语言必须是 1 到 16 个字符");
    }
    if !config.base_url.trim().is_empty() {
        canonical_loopback_origin(&config.base_url)?;
    }
    Ok(())
}

fn availability(
    config: &AudioTranscriptionConfig,
    credential_status: CredentialStatus,
    endpoint_ready: bool,
) -> String {
    if !config.enabled {
        return "disabled".to_owned();
    }
    match (credential_status, endpoint_ready) {
        (CredentialStatus::Configured, false) => "endpoint_unavailable".to_owned(),
        (CredentialStatus::Configured, true) => "ready".to_owned(),
        (CredentialStatus::Missing, _) => "missing_credential".to_owned(),
        (CredentialStatus::Unavailable, _) => "credential_unavailable".to_owned(),
    }
}

fn runtime_error(status: StatusCode, payload: &Value) -> String {
    let code = payload
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match (status.as_u16(), code) {
        (401, _) => "本地转写令牌无效或已失效".to_owned(),
        (429, "queue_full" | "app_queue_full") => "本地转写队列繁忙，请稍后重试".to_owned(),
        (503, "provider_unavailable") => "本地转写模型暂不可用".to_owned(),
        (504, "deadline_exceeded") => "本地转写超时，请缩短录音后重试".to_owned(),
        (_, "cancelled") => "本地转写已取消".to_owned(),
        _ => format!(
            "本地转写失败（HTTP {}{}）",
            status.as_u16(),
            if code.is_empty() {
                "".to_owned()
            } else {
                format!(" · {code}")
            }
        ),
    }
}

async fn persist_config(path: &PathBuf, config: &AudioTranscriptionConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.with_context(|| {
            format!("create audio transcription directory {}", parent.display())
        })?;
    }
    let temporary = path.with_extension("toml.tmp");
    fs::write(
        &temporary,
        toml::to_string_pretty(config).context("encode audio transcription configuration")?,
    )
    .await
    .with_context(|| format!("write audio transcription configuration {}", path.display()))?;
    fs::rename(&temporary, path).await.with_context(|| {
        format!(
            "replace audio transcription configuration {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_loopback_infer_runtime_addresses() {
        assert!(transcription_endpoint("http://127.0.0.1:8787").is_ok());
        assert!(transcription_endpoint("http://localhost:8787").is_err());
        assert!(transcription_endpoint("http://127.0.0.1:8787/api").is_err());
        assert!(transcription_endpoint("https://example.com").is_err());
    }

    #[test]
    fn keeps_error_messages_actionable_without_provider_text() {
        assert_eq!(
            runtime_error(
                StatusCode::TOO_MANY_REQUESTS,
                &json!({"error": {"code": "queue_full"}})
            ),
            "本地转写队列繁忙，请稍后重试"
        );
        assert_eq!(
            runtime_error(
                StatusCode::UNAUTHORIZED,
                &json!({"error": {"message": "secret"}})
            ),
            "本地转写令牌无效或已失效"
        );
    }
}
