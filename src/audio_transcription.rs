//! Local speech-to-text boundary backed by infer-runtime.
//!
//! This owner keeps microphone payloads out of conversation persistence.  It
//! accepts one short-lived upload, forwards it only to a loopback
//! infer-runtime instance, and returns editable text to the browser.  The
//! browser owns recording and discard; PCP only sees text the user sends.

use std::{collections::BTreeMap, io::ErrorKind, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use infer_runtime_client::{Error as SdkError, TranscriptionFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};
use tokio::{fs, sync::RwLock, time};

use crate::{
    infer_runtime::InferRuntimeAccess,
    secrets::{CredentialStatus, CredentialStore},
};

pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTranscriptionConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Legacy settings override. Read for migration, but never persisted or selected.
    #[serde(default, skip_serializing)]
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
    runtime: Arc<InferRuntimeAccess>,
}

impl AudioTranscriptionStore {
    pub async fn open(config_path: PathBuf, runtime: Arc<InferRuntimeAccess>) -> Result<Self> {
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
        retire_settings_endpoint_override(&mut config);
        persist_config(&config_path, &config).await?;
        runtime.set_credential_store(config.credential_store).await;
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            runtime,
        })
    }

    pub async fn snapshot(&self) -> AudioTranscriptionSnapshot {
        let config = self.config.read().await.clone();
        let credential_status = self.runtime.credential_status().await;
        let endpoint = self.runtime.resolve_endpoint();
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
                    Some(endpoint.instance_id.clone()),
                    Some(endpoint.generation.clone()),
                    None,
                )
            })
            .unwrap_or_else(|_| (String::new(), "unavailable".to_owned(), None, None, None));
        AudioTranscriptionSnapshot {
            availability: availability(&config, credential_status, endpoint_ready),
            active_credential_store: self.runtime.active_credential_store().await,
            credential_status: credential_status.as_str().to_owned(),
            debug_credential_override: self.runtime.debug_credential_override().await,
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
        retire_settings_endpoint_override(&mut config);
        validate_config(&config)?;
        if let Some(secret) = config.credential_value.as_deref() {
            self.runtime
                .write_credential(config.credential_store, secret)
                .await?;
        }
        config.credential_value = None;
        persist_config(&self.config_path, &config).await?;
        self.runtime
            .set_credential_store(config.credential_store)
            .await;
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
        let client = self
            .runtime
            .client()
            .await
            .map_err(|_| anyhow::anyhow!("本地语音转写尚未配置访问令牌或服务不可用"))?;
        let file_name = filename
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("voice-input.webm");
        let content_type = transcription_content_type(mime_type, file_name)?;
        let staged = StagedAudio::write(file_name, content_type, &audio).await?;
        let metadata = BTreeMap::from([
            ("infer.priority".to_owned(), "interactive".to_owned()),
            ("infer.placement".to_owned(), "local_only".to_owned()),
            ("infer.prefer".to_owned(), "local".to_owned()),
            ("infer.offline_required".to_owned(), "true".to_owned()),
            ("infer.fallback".to_owned(), "none".to_owned()),
        ]);
        let payload = time::timeout(
            REQUEST_TIMEOUT,
            client.sdk().transcribe_file(
                &staged.path,
                content_type,
                Some(&config.language),
                TranscriptionFormat::VerboseJson,
                &metadata,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("本地转写超时，请缩短录音后重试"))?
        .map_err(|error| anyhow::anyhow!(transcription_error(&error)))?;
        let text = payload.text.trim().to_owned();
        if text.is_empty() {
            anyhow::bail!("本地转写没有返回可用文本");
        }
        let result = TranscriptionResult {
            id: payload
                .extra
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("local-transcription")
                .to_owned(),
            model: payload
                .extra
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

struct StagedAudio {
    path: PathBuf,
    _file: NamedTempFile,
    _directory: TempDir,
}

impl StagedAudio {
    async fn write(file_name: &str, content_type: &'static str, audio: &[u8]) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("symbiont-audio-sdk-")
            .tempdir()
            .context("create private transient audio directory")?;
        restrict_transient_directory(directory.path())?;
        let suffix = audio_suffix(file_name, content_type);
        let file = tempfile::Builder::new()
            .prefix("voice-")
            .suffix(suffix)
            .tempfile_in(directory.path())
            .context("create private transient audio file")?;
        let path = file.path().to_path_buf();
        fs::write(&path, audio)
            .await
            .context("stage transient audio for official Infer Runtime SDK")?;
        Ok(Self {
            path,
            _file: file,
            _directory: directory,
        })
    }
}

#[cfg(unix)]
fn restrict_transient_directory(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict transient audio directory {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_transient_directory(_path: &std::path::Path) -> Result<()> {
    anyhow::bail!("private transient audio files require Unix owner-only permissions")
}

fn transcription_content_type(mime_type: Option<&str>, file_name: &str) -> Result<&'static str> {
    let mime_type = mime_type.map(str::trim).filter(|value| !value.is_empty());
    let content_type = match mime_type {
        Some("audio/webm") | Some("audio/webm;codecs=opus") => "audio/webm",
        Some("audio/ogg") | Some("audio/ogg;codecs=opus") => "audio/ogg",
        Some("audio/mp4") => "audio/mp4",
        Some("audio/mpeg") => "audio/mpeg",
        Some("audio/wav") | Some("audio/x-wav") => "audio/wav",
        Some("audio/flac") => "audio/flac",
        Some(value) => anyhow::bail!("不支持的录音格式：{value}"),
        None => match file_name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("webm") => "audio/webm",
            Some("ogg" | "opus") => "audio/ogg",
            Some("m4a" | "mp4") => "audio/mp4",
            Some("mp3" | "mpeg") => "audio/mpeg",
            Some("wav") => "audio/wav",
            Some("flac") => "audio/flac",
            _ => "application/octet-stream",
        },
    };
    Ok(content_type)
}

fn audio_suffix(file_name: &str, content_type: &str) -> &'static str {
    match content_type {
        "audio/webm" => ".webm",
        "audio/ogg" => ".ogg",
        "audio/mp4" => ".m4a",
        "audio/mpeg" => ".mp3",
        "audio/wav" => ".wav",
        "audio/flac" => ".flac",
        _ if file_name.ends_with(".bin") => ".bin",
        _ => ".audio",
    }
}

fn transcription_error(error: &SdkError) -> String {
    match error {
        SdkError::Api { status, code, .. } => transcription_api_error(status.as_u16(), code),
        SdkError::ContractMismatch => "本地转写服务合同尚未切换到正式版本".to_owned(),
        SdkError::Credential(_) => "本地转写令牌不可用".to_owned(),
        SdkError::Discovery(_) | SdkError::Transport(_) => "本地转写服务暂不可用".to_owned(),
        SdkError::Input(_) => "录音请求格式不受支持".to_owned(),
        SdkError::MalformedResponse(_) => "本地转写返回了无效结果".to_owned(),
    }
}

fn transcription_api_error(status: u16, code: &str) -> String {
    match (status, code) {
        (401, _) => "本地转写令牌无效或已失效".to_owned(),
        (429, "queue_full" | "app_queue_full") => "本地转写队列繁忙，请稍后重试".to_owned(),
        (503, "provider_unavailable") => "本地转写模型暂不可用".to_owned(),
        (504, "deadline_exceeded") => "本地转写超时，请缩短录音后重试".to_owned(),
        (_, "cancelled") => "本地转写已取消".to_owned(),
        _ => format!(
            "本地转写失败（HTTP {status}{}）",
            if code.is_empty() {
                String::new()
            } else {
                format!(" · {code}")
            }
        ),
    }
}

fn validate_config(config: &AudioTranscriptionConfig) -> Result<()> {
    let language = config.language.trim();
    if language.is_empty() || language.chars().count() > 16 {
        anyhow::bail!("语音识别语言必须是 1 到 16 个字符");
    }
    Ok(())
}

fn retire_settings_endpoint_override(config: &mut AudioTranscriptionConfig) {
    if !config.base_url.trim().is_empty() {
        tracing::info!(
            target: crate::runtime_log::TARGET,
            event = "infer_runtime_settings_override_retired",
            "retired the legacy infer-runtime settings override; endpoint selection is automatic"
        );
        config.base_url.clear();
    }
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
    use crate::infer_runtime::sdk_fixture::{CapturedBody, spawn};

    #[test]
    fn accepts_browser_audio_types_and_rejects_arbitrary_multipart_types() {
        assert_eq!(
            transcription_content_type(Some("audio/webm;codecs=opus"), "voice.webm").unwrap(),
            "audio/webm"
        );
        assert_eq!(
            transcription_content_type(None, "voice.m4a").unwrap(),
            "audio/mp4"
        );
        assert!(transcription_content_type(Some("text/plain"), "voice.webm").is_err());
    }

    #[test]
    fn keeps_error_messages_actionable_without_provider_text() {
        assert_eq!(
            transcription_api_error(429, "queue_full"),
            "本地转写队列繁忙，请稍后重试"
        );
        assert_eq!(transcription_api_error(401, ""), "本地转写令牌无效或已失效");
    }

    #[tokio::test]
    async fn transient_sdk_audio_file_is_private_and_removed_on_drop() {
        let staged = StagedAudio::write("voice.webm", "audio/webm", b"fixture")
            .await
            .unwrap();
        let path = staged.path.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(staged._directory.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(staged);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn official_sdk_fake_runtime_preserves_audio_constraints_without_daemon() {
        let fake = spawn().await;
        let temporary = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            InferRuntimeAccess::open_for_test(
                temporary.path().join("infer-runtime-secrets.toml"),
                &fake.endpoint,
            )
            .await
            .unwrap(),
        );
        let store =
            AudioTranscriptionStore::open(temporary.path().join("infer-runtime.toml"), runtime)
                .await
                .unwrap();
        store
            .update(AudioTranscriptionConfig {
                enabled: true,
                credential_store: CredentialStore::ConfigFile,
                credential_value: Some("fixture-token".to_owned()),
                language: "zh".to_owned(),
                ..AudioTranscriptionConfig::default()
            })
            .await
            .unwrap();

        let result = store
            .transcribe(
                Some("voice.webm"),
                Some("audio/webm;codecs=opus"),
                b"bounded-audio-fixture".to_vec(),
            )
            .await
            .unwrap();

        assert_eq!(result.id, "transcription_fixture");
        assert_eq!(result.model, "audio.transcribe");
        assert_eq!(result.text, "fixture transcript");
        let observations = fake.observations();
        assert!(observations.iter().all(|observation| {
            observation.core_contract.as_deref() == Some(infer_runtime_client::CONSUMER_CORE)
        }));
        let transcription = observations
            .iter()
            .find(|observation| observation.path == "/v1/audio/transcriptions")
            .unwrap();
        assert_eq!(
            transcription.capability_contract.as_deref(),
            Some("infer.audio.transcription@20260811.1")
        );
        assert!(transcription.authorized);
        let CapturedBody::Multipart(multipart) = &transcription.body else {
            panic!("audio fixture did not capture multipart")
        };
        for expected in [
            "audio.transcribe",
            "verbose_json",
            "infer.priority",
            "interactive",
            "infer.placement",
            "local_only",
            "infer.offline_required",
            "infer.fallback",
            "none",
        ] {
            assert!(multipart.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn retires_the_legacy_settings_endpoint_override() {
        let mut config = AudioTranscriptionConfig {
            base_url: "http://127.0.0.1:9999".to_owned(),
            ..AudioTranscriptionConfig::default()
        };

        retire_settings_endpoint_override(&mut config);

        assert!(config.base_url.is_empty());
        assert!(!toml::to_string(&config).unwrap().contains("baseUrl"));
    }
}
