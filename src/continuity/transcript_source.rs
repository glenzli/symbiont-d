use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use crate::{
    memory::MemoryRole,
    transcript::{
        TranscriptRecall, TranscriptSourceOptions,
        TranscriptSourceResolution as LocalTranscriptSourceResolution, TranscriptSourceStatus,
        TranscriptStore,
    },
};

const TRANSCRIPT_PROVIDER_ID: &str = "symbiont:transcript";
const TRANSCRIPT_LOCATOR_PREFIX: &str = "message/";
const MAX_CONTEXT_MESSAGES_PER_SIDE: u64 = 2;
const MAX_TARGET_CONTENT_CHARS: usize = 6_000;
const MAX_CONTEXT_CONTENT_CHARS: usize = 1_500;
const MAX_TOTAL_CONTENT_CHARS: usize = 12_000;
const MAX_MESSAGE_ID_CHARS: usize = 128;

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
    #[serde(default, skip_serializing_if = "is_false")]
    truncated: bool,
}

#[derive(Clone, Debug)]
struct ParsedTranscriptSource {
    locator: String,
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
    let local = TranscriptRecall::new(transcript)
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
    Ok(host_resolution(source, local))
}

fn parse_source(provider_id: &str, locator: &str) -> Result<ParsedTranscriptSource> {
    anyhow::ensure!(
        provider_id == TRANSCRIPT_PROVIDER_ID,
        "unsupported SourceRef provider; only symbiont:transcript is local"
    );
    let Some(message_id) = locator.strip_prefix(TRANSCRIPT_LOCATOR_PREFIX) else {
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
        truncated: local.truncated,
    }
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
        memory::{MemoryEntry, MemoryRole, MessagePart},
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
        assert!(parse_source("web", "message/msg_abc").is_err());
        assert!(
            parse_source("symbiont:transcript", "https://example.com/message/msg_abc").is_err()
        );
        assert!(parse_source("symbiont:transcript", "message/../private").is_err());
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

        let unavailable = resolve(store, "symbiont:transcript", "message/msg_missing", 0, 0)
            .await
            .expect("resolve missing source");
        assert_eq!(unavailable.status, TranscriptSourceStatus::Unavailable);
        assert!(unavailable.messages.is_empty());
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
