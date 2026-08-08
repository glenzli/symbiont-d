use std::{collections::BTreeMap, env, io::ErrorKind, path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    fs,
    sync::{RwLock, mpsc, watch},
};
use tracing::warn;

use crate::{
    codex::RuntimeEvent,
    sensing::{InputRoleSnapshot, SensingCandidateDraft, validate_candidate_drafts},
    usage::InvocationRecord,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-5-mini";
const DEFAULT_KEY_ENV: &str = "SYMBIONT_AMBIENT_API_KEY";
const DEFAULT_WEB_SEARCH_TOOL: &str = "web_search";

/// A connection and secret reference. Providers never decide what gets
/// explored; channels do. This prevents a failed provider from being silently
/// substituted by another model with a different perspective.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientProviderConfig {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    pub base_url: String,
    pub api_key_env: String,
    pub web_search_tool: String,
}

/// An independent input role and its observation remit.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientChannelConfig {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    pub provider_id: String,
    pub name: String,
    pub model: String,
    pub focus: String,
    pub interval_minutes: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientConfig {
    pub providers: Vec<AmbientProviderConfig>,
    pub channels: Vec<AmbientChannelConfig>,
}

impl Default for AmbientConfig {
    fn default() -> Self {
        Self {
            providers: vec![AmbientProviderConfig {
                id: "openai".to_owned(),
                enabled: false,
                base_url: DEFAULT_BASE_URL.to_owned(),
                api_key_env: DEFAULT_KEY_ENV.to_owned(),
                web_search_tool: DEFAULT_WEB_SEARCH_TOOL.to_owned(),
            }],
            channels: vec![AmbientChannelConfig {
                id: "openai-general".to_owned(),
                enabled: true,
                provider_id: "openai".to_owned(),
                name: "OpenAI · 广域观察".to_owned(),
                model: DEFAULT_MODEL.to_owned(),
                focus: "Look for recent AI releases, developer tools, independent evaluation, ecosystem shifts, and useful applications. Prefer a concrete tension over routine announcements.".to_owned(),
                interval_minutes: 180,
            }],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AmbientRuntimeDocument {
    #[serde(default)]
    channels: BTreeMap<String, AmbientChannelRuntime>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AmbientChannelRuntime {
    last_started_at: Option<String>,
    last_succeeded_at: Option<String>,
    last_failed_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientProviderSnapshot {
    pub id: String,
    pub enabled: bool,
    pub base_url: String,
    pub api_key_env: String,
    pub web_search_tool: String,
    pub availability: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientChannelSnapshot {
    #[serde(flatten)]
    pub config: AmbientChannelConfig,
    pub availability: String,
    pub last_started_at: Option<String>,
    pub last_succeeded_at: Option<String>,
    pub last_failed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientSnapshot {
    pub providers: Vec<AmbientProviderSnapshot>,
    pub channels: Vec<AmbientChannelSnapshot>,
}

pub struct AmbientTopologyStore {
    config_path: PathBuf,
    runtime_path: PathBuf,
    config: RwLock<AmbientConfig>,
    runtime: RwLock<AmbientRuntimeDocument>,
}

impl AmbientTopologyStore {
    pub async fn open(config_path: PathBuf) -> Result<Self> {
        let config = match fs::read_to_string(&config_path).await {
            Ok(value) => match toml::from_str::<AmbientConfig>(&value) {
                Ok(config) if validate_config(&config).is_ok() => config,
                Ok(_) | Err(_) => {
                    warn!(path = %config_path.display(), "ambient channel configuration is invalid; using defaults");
                    AmbientConfig::default()
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => AmbientConfig::default(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read ambient configuration {}", config_path.display())
                });
            }
        };
        let runtime_path = config_path.with_extension("runtime.json");
        let runtime = read_runtime(&runtime_path).await?;
        persist_config(&config_path, &config).await?;
        Ok(Self {
            config_path,
            runtime_path,
            config: RwLock::new(config),
            runtime: RwLock::new(runtime),
        })
    }

    pub async fn snapshot(&self) -> AmbientSnapshot {
        let config = self.config.read().await.clone();
        let runtime = self.runtime.read().await.clone();
        let providers = config
            .providers
            .iter()
            .map(|provider| AmbientProviderSnapshot {
                id: provider.id.clone(),
                enabled: provider.enabled,
                base_url: provider.base_url.clone(),
                api_key_env: provider.api_key_env.clone(),
                web_search_tool: provider.web_search_tool.clone(),
                availability: provider_availability(provider),
            })
            .collect::<Vec<_>>();
        let channels = config
            .channels
            .iter()
            .map(|channel| {
                let state = runtime
                    .channels
                    .get(&channel.id)
                    .cloned()
                    .unwrap_or_default();
                AmbientChannelSnapshot {
                    config: channel.clone(),
                    availability: channel_availability(channel, &config.providers),
                    last_started_at: state.last_started_at,
                    last_succeeded_at: state.last_succeeded_at,
                    last_failed_at: state.last_failed_at,
                    last_error: state.last_error,
                }
            })
            .collect();
        AmbientSnapshot {
            providers,
            channels,
        }
    }

    pub async fn update(&self, config: AmbientConfig) -> Result<AmbientSnapshot> {
        validate_config(&config)?;
        persist_config(&self.config_path, &config).await?;
        *self.config.write().await = config;
        Ok(self.snapshot().await)
    }

    async fn select_due_channel(&self) -> Option<(AmbientChannelConfig, AmbientProviderConfig)> {
        let config = self.config.read().await.clone();
        let runtime = self.runtime.read().await.clone();
        config
            .channels
            .iter()
            .filter_map(|channel| {
                let provider = config
                    .providers
                    .iter()
                    .find(|provider| provider.id == channel.provider_id)?;
                (channel_availability(channel, &config.providers) == "ready")
                    .then_some((channel, provider))
            })
            .filter(|(channel, _)| due(channel, runtime.channels.get(&channel.id)))
            .min_by_key(|(channel, _)| {
                runtime
                    .channels
                    .get(&channel.id)
                    .and_then(|state| state.last_started_at.clone())
            })
            .map(|(channel, provider)| (channel.clone(), provider.clone()))
    }

    async fn mark_started(&self, channel_id: &str) -> Result<()> {
        self.update_runtime(channel_id, |state| {
            state.last_started_at = Some(timestamp(Utc::now()))
        })
        .await
    }

    async fn mark_succeeded(&self, channel_id: &str) -> Result<()> {
        self.update_runtime(channel_id, |state| {
            state.last_succeeded_at = Some(timestamp(Utc::now()));
            state.last_error = None;
        })
        .await
    }

    async fn mark_failed(&self, channel_id: &str, error: &str) -> Result<()> {
        let error = error.chars().take(600).collect::<String>();
        self.update_runtime(channel_id, move |state| {
            state.last_failed_at = Some(timestamp(Utc::now()));
            state.last_error = Some(error);
        })
        .await
    }

    async fn update_runtime(
        &self,
        channel_id: &str,
        update: impl FnOnce(&mut AmbientChannelRuntime),
    ) -> Result<()> {
        let mut runtime = self.runtime.write().await;
        update(runtime.channels.entry(channel_id.to_owned()).or_default());
        let snapshot = runtime.clone();
        drop(runtime);
        persist_runtime(&self.runtime_path, &snapshot).await
    }
}

#[derive(Clone)]
pub struct AmbientScout {
    topology: Arc<AmbientTopologyStore>,
    client: Client,
}

pub struct AmbientSenseOutcome {
    pub candidates: Vec<SensingCandidateDraft>,
    pub invocation: Option<InvocationRecord>,
    pub actor: Option<InputRoleSnapshot>,
    pub interrupted: bool,
    pub channel_failure: Option<String>,
}

impl AmbientScout {
    pub fn new(topology: Arc<AmbientTopologyStore>) -> Result<Self> {
        Ok(Self {
            topology,
            client: Client::builder()
                .build()
                .context("build ambient API client")?,
        })
    }

    pub async fn sense(
        &self,
        sensing_context: &str,
        mut input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<AmbientSenseOutcome> {
        let Some((channel, provider)) = self.topology.select_due_channel().await else {
            return Ok(empty_outcome());
        };
        self.topology.mark_started(&channel.id).await?;
        let result = self
            .call(
                &channel,
                &provider,
                sensing_context,
                &mut input_events,
                events,
            )
            .await;
        match result {
            Ok(outcome) if outcome.interrupted => Ok(outcome),
            Ok(outcome) => {
                self.topology.mark_succeeded(&channel.id).await?;
                Ok(outcome)
            }
            Err(error) => {
                let message = error.to_string();
                self.topology.mark_failed(&channel.id, &message).await?;
                warn!(channel = %channel.id, provider = %provider.id, %error, "ambient channel failed without fallback");
                Ok(AmbientSenseOutcome {
                    channel_failure: Some(message),
                    ..empty_outcome()
                })
            }
        }
    }

    async fn call(
        &self,
        channel: &AmbientChannelConfig,
        provider: &AmbientProviderConfig,
        sensing_context: &str,
        input_events: &mut watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<AmbientSenseOutcome> {
        let api_key = env::var(&provider.api_key_env)
            .with_context(|| format!("read ambient API key from {}", provider.api_key_env))?;
        let started = Utc::now();
        let started_instant = Instant::now();
        let request_id = format!("ambient_{}_{}", channel.id, started.timestamp_micros());
        let _ = events
            .send(RuntimeEvent::Activity {
                label: format!("{} 正在观察", channel.name),
                model: channel.model.clone(),
                display_name: channel.name.clone(),
                effort: "input-only".to_owned(),
                lane: "sense".to_owned(),
            })
            .await;
        let request = self
            .client
            .post(responses_url(&provider.base_url)?)
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .header("X-Client-Request-Id", &request_id)
            .json(&responses_request(channel, provider, sensing_context));
        let response = tokio::select! {
            result = request.send() => result.context("call ambient Responses API")?,
            changed = input_events.changed() => {
                changed.context("watch newer user input during ambient sensing")?;
                return Ok(AmbientSenseOutcome { interrupted: true, ..empty_outcome() });
            }
        };
        let status = response.status();
        let response_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let payload: Value = response
            .json()
            .await
            .context("decode ambient Responses API response")?;
        if !status.is_success() {
            anyhow::bail!(
                "ambient Responses API returned HTTP {status}: {}",
                compact_error(&payload)
            );
        }
        let candidates = response_candidates(&payload)?;
        let completed = Utc::now();
        let invocation = InvocationRecord {
            id: response_id.unwrap_or_else(|| request_id.clone()),
            parent_id: None,
            thread_id: format!("ambient-api:{}", channel.id),
            turn_id: request_id,
            origin: "ambient_sense".to_owned(),
            lane: "sense".to_owned(),
            requested_model: channel.model.clone(),
            effective_model: payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&channel.model)
                .to_owned(),
            model_display_name: channel.name.clone(),
            effort: "input-only".to_owned(),
            service_tier: None,
            started_at: timestamp(started),
            completed_at: timestamp(completed),
            duration_ms: started_instant.elapsed().as_millis() as u64,
            status: "completed".to_owned(),
            input_tokens: token(&payload, "/usage/input_tokens"),
            cached_input_tokens: token(&payload, "/usage/input_tokens_details/cached_tokens"),
            output_tokens: token(&payload, "/usage/output_tokens"),
            reasoning_output_tokens: token(
                &payload,
                "/usage/output_tokens_details/reasoning_tokens",
            ),
            total_tokens: token(&payload, "/usage/total_tokens"),
            tool_calls: vec![
                provider.web_search_tool.clone(),
                "submit_sensing_candidates".to_owned(),
            ],
            produced_message: false,
            trace_steps: Vec::new(),
            context_snapshot: None,
            trace_events: Vec::new(),
        };
        Ok(AmbientSenseOutcome {
            candidates,
            invocation: Some(invocation),
            actor: Some(InputRoleSnapshot::ambient(
                &channel.id,
                &channel.name,
                &channel.model,
                &provider.id,
            )),
            interrupted: false,
            channel_failure: None,
        })
    }
}

fn empty_outcome() -> AmbientSenseOutcome {
    AmbientSenseOutcome {
        candidates: Vec::new(),
        invocation: None,
        actor: None,
        interrupted: false,
        channel_failure: None,
    }
}

fn responses_request(
    channel: &AmbientChannelConfig,
    provider: &AmbientProviderConfig,
    context: &str,
) -> Value {
    json!({"model":channel.model,"store":false,"instructions":sensing_instructions(),"input":[{"role":"user","content":[{"type":"input_text","text":format!("<channel id=\"{}\" name=\"{}\">\nfocus: {}\n</channel>\n{}", channel.id, channel.name, channel.focus, context)}]}],"tools":[{"type":provider.web_search_tool},{"type":"function","name":"submit_sensing_candidates","description":"Place one to three credible external-signal candidates into the private transient intake pool.","strict":true,"parameters":sensing_tool_schema()}]})
}

fn sensing_instructions() -> &'static str {
    "Run one low-cost ambient sensing pass for the supplied independent input role. Follow its focus, but do not pretend it defines the user's interests. Use web search for credible fresh or recently active external developments with information or discussion value. Later independent evaluation, adoption, failure, or a clearer interpretive tension can make an event timely again. Prefer primary or authoritative sources for facts and credible independent sources for reception and reproducibility. This is intake only: do not create memory, alter context, or write user-visible prose. If and only if one to three candidates have concrete sources, call submit_sensing_candidates once. proposed_input must be a self-contained natural two-to-four sentence input in your own voice, stating the object, essential evidence or uncertainty, and the tension worth noticing. It is valid to submit nothing."
}
fn sensing_tool_schema() -> Value {
    json!({"type":"object","properties":{"candidates":{"type":"array","minItems":1,"maxItems":3,"items":{"type":"object","properties":{"title":{"type":"string","maxLength":240},"summary":{"type":"string","maxLength":1000},"proposed_input":{"type":"string","maxLength":1800},"event_at":{"type":"string","maxLength":64},"source_class":{"type":"string","enum":["research","products_and_tools","projects_and_ecosystems","institutions_and_policy","industry_and_markets","culture_and_ideas","open_discovery"]},"possible_connection":{"type":"string","maxLength":800},"sources":{"type":"array","minItems":1,"maxItems":3,"items":{"type":"object","properties":{"url":{"type":"string","maxLength":900},"detail":{"type":"string","maxLength":800}},"required":["url","detail"],"additionalProperties":false}}},"required":["title","summary","proposed_input","source_class","sources"],"additionalProperties":false}}},"required":["candidates"],"additionalProperties":false})
}
fn response_candidates(response: &Value) -> Result<Vec<SensingCandidateDraft>> {
    let mut calls = response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some("submit_sensing_candidates")
        });
    let Some(call) = calls.next() else {
        return Ok(Vec::new());
    };
    if calls.next().is_some() {
        anyhow::bail!("ambient Responses API returned more than one candidate handoff");
    }
    let arguments = call
        .get("arguments")
        .and_then(Value::as_str)
        .context("ambient candidate handoff omitted arguments")?;
    let candidates = serde_json::from_str::<Value>(arguments)
        .context("decode ambient candidate handoff")?
        .get("candidates")
        .cloned()
        .context("ambient candidate handoff omitted candidates")?;
    let candidates: Vec<SensingCandidateDraft> =
        serde_json::from_value(candidates).context("decode ambient sensing candidates")?;
    validate_candidate_drafts(&candidates)?;
    Ok(candidates)
}

fn validate_config(config: &AmbientConfig) -> Result<()> {
    if config.providers.is_empty()
        || config.providers.len() > 12
        || config.channels.is_empty()
        || config.channels.len() > 24
    {
        anyhow::bail!("ambient configuration requires 1–12 providers and 1–24 channels");
    }
    let mut provider_ids = BTreeMap::new();
    for provider in &config.providers {
        validate_id(&provider.id, "provider")?;
        if provider_ids.insert(provider.id.as_str(), ()).is_some() {
            anyhow::bail!("ambient provider ids must be unique");
        }
        let url = Url::parse(provider.base_url.trim()).context("parse ambient API base URL")?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            anyhow::bail!("ambient API base URL must be an http(s) origin without credentials");
        }
        validate_env(&provider.api_key_env)?;
        if provider.web_search_tool.trim().is_empty()
            || provider.web_search_tool.chars().count() > 80
        {
            anyhow::bail!("ambient web-search tool type must contain at most 80 characters");
        }
    }
    let mut channel_ids = BTreeMap::new();
    for channel in &config.channels {
        validate_id(&channel.id, "channel")?;
        if channel_ids.insert(channel.id.as_str(), ()).is_some() {
            anyhow::bail!("ambient channel ids must be unique");
        }
        if !provider_ids.contains_key(channel.provider_id.as_str()) {
            anyhow::bail!(
                "ambient channel {} references an unknown provider",
                channel.id
            );
        }
        for (field, value, limit) in [
            ("name", channel.name.as_str(), 80),
            ("model", channel.model.as_str(), 160),
            ("focus", channel.focus.as_str(), 1_600),
        ] {
            if value.trim().is_empty() || value.chars().count() > limit {
                anyhow::bail!("ambient channel {field} must contain at most {limit} characters");
            }
        }
        if !(5..=10_080).contains(&channel.interval_minutes) {
            anyhow::bail!("ambient channel interval must be between 5 minutes and 7 days");
        }
    }
    Ok(())
}

fn validate_id(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        anyhow::bail!("ambient {kind} id must use lowercase letters, digits, and hyphens");
    }
    Ok(())
}
fn validate_env(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        anyhow::bail!(
            "ambient API key environment variable must use uppercase letters, digits, and underscores"
        );
    }
    Ok(())
}
fn provider_availability(provider: &AmbientProviderConfig) -> String {
    if !provider.enabled {
        "disabled".to_owned()
    } else if env::var(&provider.api_key_env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_none()
    {
        format!("missing_key_env:{}", provider.api_key_env)
    } else {
        "ready".to_owned()
    }
}
fn channel_availability(
    channel: &AmbientChannelConfig,
    providers: &[AmbientProviderConfig],
) -> String {
    if !channel.enabled {
        return "disabled".to_owned();
    }
    providers
        .iter()
        .find(|provider| provider.id == channel.provider_id)
        .map(provider_availability)
        .unwrap_or_else(|| "unknown_provider".to_owned())
}
fn due(channel: &AmbientChannelConfig, runtime: Option<&AmbientChannelRuntime>) -> bool {
    runtime
        .and_then(|state| state.last_started_at.as_deref())
        .and_then(parse_time)
        .is_none_or(|last| {
            Utc::now() - last >= chrono::Duration::minutes(channel.interval_minutes as i64)
        })
}
fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}
fn responses_url(base_url: &str) -> Result<Url> {
    Url::parse(&format!("{}/responses", base_url.trim_end_matches('/')))
        .context("build ambient Responses API URL")
}
fn compact_error(value: &Value) -> String {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("unknown response error")
        .chars()
        .take(400)
        .collect()
}
fn token(value: &Value, pointer: &str) -> u64 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or_default()
}
fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn read_runtime(path: &PathBuf) -> Result<AmbientRuntimeDocument> {
    match fs::read_to_string(path).await {
        Ok(value) => serde_json::from_str(&value).context("decode ambient channel runtime"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(AmbientRuntimeDocument::default()),
        Err(error) => {
            Err(error).with_context(|| format!("read ambient channel runtime {}", path.display()))
        }
    }
}
async fn persist_config(path: &PathBuf, config: &AmbientConfig) -> Result<()> {
    persist(
        path,
        toml::to_string_pretty(config).context("encode ambient configuration")?,
    )
    .await
}
async fn persist_runtime(path: &PathBuf, runtime: &AmbientRuntimeDocument) -> Result<()> {
    persist(
        path,
        serde_json::to_string_pretty(runtime).context("encode ambient channel runtime")?,
    )
    .await
}
async fn persist(path: &PathBuf, content: String) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "create ambient configuration directory {}",
                parent.display()
            )
        })?;
    }
    let temporary = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
    ));
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write ambient configuration {}", path.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace ambient configuration {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_the_responses_function_handoff() {
        let arguments = json!({"candidates":[{"title":"A signal","summary":"A fact","proposed_input":"An input","source_class":"research","sources":[{"url":"https://example.test","detail":"Primary"}]}]}).to_string();
        let response = json!({"output":[{"type":"function_call","name":"submit_sensing_candidates","arguments":arguments}]});
        assert_eq!(response_candidates(&response).unwrap().len(), 1);
    }
    #[test]
    fn rejects_channels_with_unknown_providers() {
        let mut config = AmbientConfig::default();
        config.channels[0].provider_id = "missing".to_owned();
        assert!(validate_config(&config).is_err());
    }
    #[test]
    fn channel_due_uses_its_own_schedule() {
        let channel = AmbientConfig::default().channels.remove(0);
        assert!(due(&channel, None));
        let state = AmbientChannelRuntime {
            last_started_at: Some(timestamp(Utc::now())),
            ..Default::default()
        };
        assert!(!due(&channel, Some(&state)));
    }
}
