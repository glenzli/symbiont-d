//! Local speech-to-text boundary backed by infer-runtime.
//!
//! This owner keeps microphone payloads out of conversation persistence.  It
//! accepts one short-lived upload, forwards it only to a loopback
//! infer-runtime instance, and returns editable text to the browser.  The
//! browser owns recording and discard; PCP only sees text the user sends.

use std::{io::ErrorKind, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode, Url, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{fs, sync::RwLock};

use crate::secrets::{CredentialStatus, CredentialStore, SecretStore};

const TRANSCRIPTION_SECRET_ID: &str = "infer-runtime";
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
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
            base_url: default_base_url(),
            credential_store: CredentialStore::ConfigFile,
            credential_value: None,
            language: default_language(),
        }
    }
}

fn default_base_url() -> String {
    "http://127.0.0.1:8787".to_owned()
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
}

impl AudioTranscriptionStore {
    pub async fn open(config_path: PathBuf) -> Result<Self> {
        let config = match fs::read_to_string(&config_path).await {
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
        persist_config(&config_path, &config).await?;
        let credential_path = config_path.with_file_name("infer-runtime-secrets.toml");
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            credentials: SecretStore::open(credential_path).await?,
            client: Client::builder().timeout(REQUEST_TIMEOUT).build()?,
        })
    }

    pub async fn snapshot(&self) -> AudioTranscriptionSnapshot {
        let config = self.config.read().await.clone();
        let credential_status = self
            .credentials
            .status(TRANSCRIPTION_SECRET_ID, config.credential_store)
            .await;
        AudioTranscriptionSnapshot {
            availability: availability(&config, credential_status),
            active_credential_store: self.credentials.active_store(config.credential_store),
            credential_status: credential_status.as_str().to_owned(),
            debug_credential_override: self.credentials.debug_override(config.credential_store),
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
        let endpoint = transcription_endpoint(&config.base_url)?;
        let file_name = filename
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("voice-input.webm");
        let mut part = multipart::Part::bytes(audio).file_name(file_name.to_owned());
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
            .text("language", config.language)
            .text("response_format", "verbose_json")
            .text("metadata", metadata.to_string());
        let response = self
            .client
            .post(endpoint)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .context("contact local infer-runtime")?;
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
}

fn transcription_endpoint(base_url: &str) -> Result<Url> {
    let mut url = Url::parse(base_url.trim()).context("parse infer-runtime address")?;
    let host = url.host_str().unwrap_or_default();
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        anyhow::bail!("infer-runtime 地址必须是本机回环地址");
    }
    url.set_path("/v1/audio/transcriptions");
    url.set_query(None);
    Ok(url)
}

fn validate_config(config: &AudioTranscriptionConfig) -> Result<()> {
    let language = config.language.trim();
    if language.is_empty() || language.chars().count() > 16 {
        anyhow::bail!("语音识别语言必须是 1 到 16 个字符");
    }
    transcription_endpoint(&config.base_url)?;
    Ok(())
}

fn availability(config: &AudioTranscriptionConfig, credential_status: CredentialStatus) -> String {
    if !config.enabled {
        return "disabled".to_owned();
    }
    match credential_status {
        CredentialStatus::Configured => "ready".to_owned(),
        CredentialStatus::Missing => "missing_credential".to_owned(),
        CredentialStatus::Unavailable => "credential_unavailable".to_owned(),
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
        assert!(transcription_endpoint("http://localhost:8787/api").is_ok());
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
