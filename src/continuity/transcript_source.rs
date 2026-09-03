use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{
    memory::{MemoryRole, MessageExternalInputReference, MessagePart},
    transcript::{
        TranscriptRecall, TranscriptSourceOptions,
        TranscriptSourceResolution as LocalTranscriptSourceResolution, TranscriptSourceStatus,
        TranscriptStore,
    },
};

const TRANSCRIPT_PROVIDER_ID: &str = "symbiont:transcript";
const TRANSCRIPT_LOCATOR_PREFIX: &str = "message/";
const TRANSCRIPT_STORE_LOCATOR_PREFIX: &str = "store/";
const MAX_CONTEXT_MESSAGES_PER_SIDE: u64 = 2;
const MAX_TARGET_CONTENT_CHARS: usize = 6_000;
const MAX_CONTEXT_CONTENT_CHARS: usize = 1_500;
const MAX_TOTAL_CONTENT_CHARS: usize = 12_000;
const MAX_MESSAGE_ID_CHARS: usize = 128;
const MAX_EXTERNAL_INPUT_CHARS: usize = 6_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostedTranscriptMessage {
    message_id: String,
    role: MemoryRole,
    time: String,
    content: String,
    target: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptSourceResolution {
    provider_id: String,
    locator: String,
    source_message_id: String,
    status: TranscriptSourceStatus,
    messages: Vec<HostedTranscriptMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    external_inputs: Vec<MessageExternalInputReference>,
    #[serde(default, skip_serializing_if = "is_false")]
    truncated: bool,
}

#[derive(Clone, Debug)]
struct ParsedTranscriptSource {
    locator: String,
    source_store_id: Option<String>,
    message_id: String,
}

pub(super) async fn resolve(
    transcript: Arc<TranscriptStore>,
    provider_id: &str,
    locator: &str,
    context_before: u64,
    context_after: u64,
) -> Result<TranscriptSourceResolution> {
    let source = parse_source(provider_id, locator)?;
    anyhow::ensure!(
        context_before <= MAX_CONTEXT_MESSAGES_PER_SIDE
            && context_after <= MAX_CONTEXT_MESSAGES_PER_SIDE,
        "transcript SourceRef context is limited to two messages on each side"
    );
    if source
        .source_store_id
        .as_deref()
        .is_some_and(|source_store_id| source_store_id != transcript.source_store_id())
    {
        return Ok(TranscriptSourceResolution {
            provider_id: TRANSCRIPT_PROVIDER_ID.to_owned(),
            locator: source.locator,
            source_message_id: source.message_id,
            status: TranscriptSourceStatus::Unavailable,
            messages: Vec::new(),
            external_inputs: Vec::new(),
            truncated: false,
        });
    }
    let external_inputs = transcript
        .by_ids(std::slice::from_ref(&source.message_id))
        .await?
        .into_iter()
        .next()
        .map(|entry| bounded_external_inputs(entry.parts))
        .unwrap_or_default();
    let mut local = TranscriptRecall::new(transcript)
        .resolve_source(
            &source.message_id,
            TranscriptSourceOptions {
                context_before: context_before as usize,
                context_after: context_after as usize,
                target_max_chars: MAX_TARGET_CONTENT_CHARS,
                neighbor_max_chars: MAX_CONTEXT_CONTENT_CHARS,
                max_chars: MAX_TOTAL_CONTENT_CHARS,
                ..TranscriptSourceOptions::default()
            },
        )
        .await?;
    local.external_inputs = external_inputs.0;
    local.truncated |= external_inputs.1;
    Ok(host_resolution(source, local))
}

fn parse_source(provider_id: &str, locator: &str) -> Result<ParsedTranscriptSource> {
    anyhow::ensure!(
        provider_id == TRANSCRIPT_PROVIDER_ID,
        "unsupported SourceRef provider; only symbiont:transcript is local"
    );
    let (source_store_id, message_id) =
        if let Some(message_id) = locator.strip_prefix(TRANSCRIPT_LOCATOR_PREFIX) {
            // Historical SourceRefs predate explicit Host source-store identity.
            (None, message_id)
        } else if let Some(rest) = locator.strip_prefix(TRANSCRIPT_STORE_LOCATOR_PREFIX) {
            let (source_store_id, message_id) = rest
                .split_once("/message/")
                .context("unsupported transcript SourceRef locator")?;
            anyhow::ensure!(
                source_store_id.starts_with("src_")
                    && source_store_id.len() == 36
                    && source_store_id
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_'),
                "transcript SourceRef source store id is invalid"
            );
            (Some(source_store_id.to_owned()), message_id)
        } else {
            anyhow::bail!("unsupported transcript SourceRef locator");
        };
    let message_id_chars = message_id.chars().count();
    anyhow::ensure!(
        (1..=MAX_MESSAGE_ID_CHARS).contains(&message_id_chars)
            && message_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            }),
        "transcript SourceRef message id is invalid"
    );
    Ok(ParsedTranscriptSource {
        locator: locator.to_owned(),
        source_store_id,
        message_id: message_id.to_owned(),
    })
}

fn host_resolution(
    source: ParsedTranscriptSource,
    local: LocalTranscriptSourceResolution,
) -> TranscriptSourceResolution {
    TranscriptSourceResolution {
        provider_id: TRANSCRIPT_PROVIDER_ID.to_owned(),
        locator: source.locator,
        source_message_id: local.source_message_id,
        status: local.status,
        messages: local
            .messages
            .into_iter()
            .map(|message| HostedTranscriptMessage {
                message_id: message.message_id,
                role: message.role,
                time: message.occurred_at,
                content: message.content,
                target: message.matched,
                truncated: message.truncated,
            })
            .collect(),
        external_inputs: local.external_inputs,
        truncated: local.truncated,
    }
}

fn bounded_external_inputs(parts: Vec<MessagePart>) -> (Vec<MessageExternalInputReference>, bool) {
    let mut remaining = MAX_EXTERNAL_INPUT_CHARS;
    let mut truncated = false;
    let mut inputs = Vec::new();
    for part in parts {
        let MessagePart::ExternalInput { mut input } = part else {
            continue;
        };
        if let Some(content) = input.content.take() {
            let chars = content.chars().count();
            let selected = content.chars().take(remaining).collect::<String>();
            remaining = remaining.saturating_sub(selected.chars().count());
            truncated |= selected.chars().count() < chars;
            input.content = Some(selected);
        }
        inputs.push(input);
        if remaining == 0 {
            break;
        }
    }
    (inputs, truncated)
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        MAX_CONTEXT_CONTENT_CHARS, MAX_TARGET_CONTENT_CHARS, TranscriptSourceStatus, parse_source,
        resolve,
    };
    use crate::{
        memory::{
            MemoryEntry, MemoryRole, MessageExternalInputReference, MessageExternalInputSource,
            MessagePart,
        },
        transcript::{TranscriptMessageLinks, TranscriptStore},
    };

    fn entry(role: MemoryRole, at: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            role,
            at: at.to_owned(),
            content: content.to_owned(),
            revision_id: None,
            parts: vec![MessagePart::Markdown {
                text: content.to_owned(),
            }],
            metadata: None,
            delivery_state: None,
        }
    }

    #[test]
    fn accepts_only_the_local_transcript_source_ref_shape() {
        assert!(parse_source("symbiont:transcript", "message/msg_abc-123").is_ok());
        assert!(
            parse_source(
                "symbiont:transcript",
                "store/src_0123456789abcdef0123456789abcdef/message/msg_abc-123"
            )
            .is_ok()
        );
        assert!(parse_source("web", "message/msg_abc").is_err());
        assert!(
            parse_source("symbiont:transcript", "https://example.com/message/msg_abc").is_err()
        );
        assert!(parse_source("symbiont:transcript", "message/../private").is_err());
        assert!(parse_source("symbiont:transcript", "store/src_wrong/message/msg_abc").is_err());
    }

    #[tokio::test]
    async fn resolves_one_source_with_only_the_requested_bounded_neighbors() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        let store = Arc::new(store);
        let first = store
            .append(
                entry(MemoryRole::User, "2026-08-31T00:00:00Z", "first"),
                TranscriptMessageLinks::default(),
            )
            .await
            .expect("append first");
        let target = store
            .append(
                entry(
                    MemoryRole::Assistant,
                    "2026-08-31T00:00:01Z",
                    &"target".repeat(MAX_TARGET_CONTENT_CHARS),
                ),
                TranscriptMessageLinks::default(),
            )
            .await
            .expect("append target");
        let third = store
            .append(
                entry(
                    MemoryRole::User,
                    "2026-08-31T00:00:02Z",
                    &"third".repeat(MAX_CONTEXT_CONTENT_CHARS),
                ),
                TranscriptMessageLinks::default(),
            )
            .await
            .expect("append third");
        store
            .append(
                entry(MemoryRole::Assistant, "2026-08-31T00:00:03Z", "fourth"),
                TranscriptMessageLinks::default(),
            )
            .await
            .expect("append fourth");

        let resolution = resolve(
            store,
            "symbiont:transcript",
            &format!("message/{}", target.message_id),
            1,
            1,
        )
        .await
        .expect("resolve source");

        assert_eq!(resolution.status, TranscriptSourceStatus::Active);
        assert_eq!(resolution.source_message_id, target.message_id);
        assert_eq!(resolution.messages.len(), 3);
        assert_eq!(resolution.messages[0].message_id, first.message_id);
        assert_eq!(resolution.messages[1].message_id, target.message_id);
        assert_eq!(resolution.messages[1].role, MemoryRole::Assistant);
        assert!(resolution.messages[1].target);
        assert_eq!(resolution.messages[1].content.chars().count(), 6_000);
        assert!(resolution.messages[1].truncated);
        assert_eq!(resolution.messages[2].message_id, third.message_id);
        assert_eq!(resolution.messages[2].content.chars().count(), 1_500);
        assert!(resolution.messages[2].truncated);
    }

    #[tokio::test]
    async fn exact_source_resolution_includes_bounded_external_reply_provenance() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        let store = Arc::new(store);
        let mut source = entry(
            MemoryRole::User,
            "2026-09-04T00:00:00Z",
            "Why does this external result matter?",
        );
        source.parts.insert(
            0,
            MessagePart::ExternalInput {
                input: MessageExternalInputReference {
                    signal_id: Some("signal_local".into()),
                    source_revision_id: None,
                    actor_name: "Gemini Spark".into(),
                    title: "A result".into(),
                    observed_at: "2026-09-04T00:00:00Z".into(),
                    excerpt: "bounded display excerpt".into(),
                    content: Some("source body".repeat(1_000)),
                    qualification_note: Some("unverified external input".into()),
                    sources: vec![MessageExternalInputSource {
                        url: "https://example.test/paper".into(),
                        detail: "paper".into(),
                    }],
                    source_count: 1,
                },
            },
        );
        let written = store
            .append(source, TranscriptMessageLinks::default())
            .await
            .expect("append source");

        let resolution = resolve(
            store,
            "symbiont:transcript",
            &format!("message/{}", written.message_id),
            0,
            0,
        )
        .await
        .expect("resolve source");

        assert_eq!(resolution.external_inputs.len(), 1);
        assert_eq!(
            resolution.external_inputs[0].signal_id.as_deref(),
            Some("signal_local")
        );
        assert_eq!(resolution.external_inputs[0].source_revision_id, None);
        assert_eq!(
            resolution.external_inputs[0].sources[0].url,
            "https://example.test/paper"
        );
        assert_eq!(
            resolution.external_inputs[0]
                .content
                .as_deref()
                .unwrap()
                .chars()
                .count(),
            6_000
        );
        assert!(resolution.truncated);
    }

    #[tokio::test]
    async fn reports_retracted_and_unavailable_sources_without_exposing_content() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        let store = Arc::new(store);
        let written = store
            .append(
                entry(MemoryRole::User, "2026-08-31T00:00:00Z", "private text"),
                TranscriptMessageLinks::default(),
            )
            .await
            .expect("append message");
        store
            .retract_from(&written.message_id)
            .await
            .expect("retract message");

        let retracted = resolve(
            Arc::clone(&store),
            "symbiont:transcript",
            &format!("message/{}", written.message_id),
            2,
            2,
        )
        .await
        .expect("resolve retracted source");
        assert_eq!(retracted.status, TranscriptSourceStatus::Retracted);
        assert!(retracted.messages.is_empty());

        let unavailable = resolve(
            Arc::clone(&store),
            "symbiont:transcript",
            "message/msg_missing",
            0,
            0,
        )
        .await
        .expect("resolve missing source");
        assert_eq!(unavailable.status, TranscriptSourceStatus::Unavailable);
        assert!(unavailable.messages.is_empty());

        let foreign = resolve(
            Arc::clone(&store),
            "symbiont:transcript",
            "store/src_0123456789abcdef0123456789abcdef/message/msg_missing",
            0,
            0,
        )
        .await
        .expect("resolve foreign source store");
        assert_eq!(foreign.status, TranscriptSourceStatus::Unavailable);
        assert!(foreign.messages.is_empty());
    }

    #[tokio::test]
    async fn rejects_context_windows_larger_than_the_host_limit() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        let error = resolve(
            Arc::new(store),
            "symbiont:transcript",
            "message/msg_valid",
            3,
            0,
        )
        .await
        .expect_err("oversized context must fail");
        assert!(error.to_string().contains("limited to two messages"));
    }
}
