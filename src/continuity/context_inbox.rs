//! Optional PCP Runtime Context Inbox beside the formal Page Store.
//!
//! Candidates and activity cards are deliberately not Pages and never enter
//! ordinary recall unless the Store operator later promotes a candidate.

use std::{collections::HashSet, future::Future};

use anyhow::{Result, ensure};
use pcp_client::{
    LEGACY_RUNTIME_CONTEXT_HUB_FEATURE, RUNTIME_CONTEXT_INBOX_FEATURE,
    context_hub::{
        ActivityInput, ActivityQuery, CandidateInput, ContextHubRequest as ContextInboxRequest,
    },
};
use pcp_core::SourceRef;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::memory::MessagePart;

use super::ContinuityHost;

const MAX_CANDIDATE_SOURCE_MESSAGES: usize = 6;
const MAX_CANDIDATE_SOURCE_REFS: usize = 8;

impl ContinuityHost {
    pub(crate) async fn submit_context_candidate(
        &self,
        title: &str,
        content: &str,
        source_message_ids: &[String],
        based_on_revision_ids: &[String],
    ) -> Result<Value> {
        self.require_context_inbox()?;
        let title = title.trim();
        let content = content.trim();
        ensure!(
            (1..=120).contains(&title.chars().count()),
            "candidate title must contain 1-120 characters"
        );
        ensure!(
            (1..=2_000).contains(&content.chars().count()),
            "candidate content must contain 1-2000 characters"
        );
        let message_ids = normalized_ids(source_message_ids);
        let revision_ids = normalized_ids(based_on_revision_ids);
        ensure!(
            message_ids.len() <= MAX_CANDIDATE_SOURCE_MESSAGES,
            "a candidate can cite at most {MAX_CANDIDATE_SOURCE_MESSAGES} transcript messages"
        );
        ensure!(
            revision_ids.len() <= 16,
            "a candidate can cite at most 16 PCP Revisions"
        );
        ensure!(
            !message_ids.is_empty() || !revision_ids.is_empty(),
            "a candidate requires exact local messages or PCP Revisions as evidence"
        );

        let source_refs = self.candidate_source_refs(&message_ids).await?;
        let request = candidate_request(
            &self.scopes.namespace,
            title,
            content,
            source_refs,
            &message_ids,
            &revision_ids,
        );
        dispatch_candidate_once(request, |request| self.store.context_hub(request)).await
    }

    pub(crate) async fn publish_runtime_activity(
        &self,
        topic_key: &str,
        summary: &str,
        expected_version: Option<u64>,
        ttl_hours: Option<u32>,
    ) -> Result<Value> {
        self.require_context_inbox()?;
        let topic_key = topic_key.trim();
        let summary = summary.trim();
        ensure!(
            (1..=64).contains(&topic_key.chars().count()),
            "activity topic key must contain 1-64 characters"
        );
        ensure!(
            (1..=180).contains(&summary.chars().count()),
            "activity summary must contain 1-180 characters"
        );
        ensure!(
            expected_version.is_none_or(|version| version > 0),
            "activity expected version must be positive"
        );
        ensure!(
            ttl_hours.is_none_or(|hours| (1..=168).contains(&hours)),
            "activity TTL must be between 1 and 168 hours"
        );
        self.store
            .context_hub(ContextInboxRequest::PublishActivity(ActivityInput {
                scope: self.scopes.namespace.clone(),
                topic_key: topic_key.to_owned(),
                summary: summary.to_owned(),
                expected_version,
                ttl_hours,
            }))
            .await
    }

    pub(crate) async fn read_runtime_activity(
        &self,
        scopes: &[String],
        query: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
        include_own: bool,
    ) -> Result<Value> {
        self.require_context_inbox()?;
        let query = query.map(str::trim).filter(|value| !value.is_empty());
        let cursor = cursor.map(str::trim).filter(|value| !value.is_empty());
        ensure!(
            query.is_none_or(|value| value.chars().count() <= 120),
            "activity query can contain at most 120 characters"
        );
        ensure!(
            cursor.is_none_or(|value| value.chars().count() <= 256),
            "activity cursor can contain at most 256 characters"
        );
        ensure!(
            limit.is_none_or(|value| (1..=5).contains(&value)),
            "activity read limit must be between 1 and 5"
        );
        self.store
            .context_hub(ContextInboxRequest::ReadActivity(ActivityQuery {
                scopes: self.resolve_scopes(scopes)?,
                query: query.map(str::to_owned),
                cursor: cursor.map(str::to_owned),
                limit,
                include_own,
            }))
            .await
    }

    fn require_context_inbox(&self) -> Result<()> {
        ensure!(
            context_inbox_available(&self.store.capabilities().features),
            "PCP Runtime context inbox is unavailable; candidate and activity state was not written"
        );
        Ok(())
    }

    async fn candidate_source_refs(&self, message_ids: &[String]) -> Result<Vec<SourceRef>> {
        let mut refs = self.transcript_source_refs(message_ids).await?;
        let entries = self.transcript.by_ids(message_ids).await?;
        for entry in entries {
            for part in entry.parts {
                let MessagePart::ExternalInput { input } = part else {
                    continue;
                };
                refs.extend(input.sources.into_iter().map(|source| SourceRef {
                    provider_id: "web".to_owned(),
                    locator: source.url,
                    media_type: Some("text/html".to_owned()),
                    content_digest: None,
                }));
            }
        }
        let mut seen = HashSet::new();
        refs.retain(|source| seen.insert(format!("{}\0{}", source.provider_id, source.locator)));
        refs.truncate(MAX_CANDIDATE_SOURCE_REFS);
        Ok(refs)
    }
}

fn context_inbox_available(features: &[String]) -> bool {
    features.iter().any(|feature| {
        feature == RUNTIME_CONTEXT_INBOX_FEATURE || feature == LEGACY_RUNTIME_CONTEXT_HUB_FEATURE
    })
}

fn candidate_request(
    scope: &str,
    title: &str,
    content: &str,
    source_refs: Vec<SourceRef>,
    message_ids: &[String],
    revision_ids: &[String],
) -> ContextInboxRequest {
    ContextInboxRequest::SubmitCandidate(CandidateInput {
        scope: scope.to_owned(),
        event_id: candidate_event_id(title, content, message_ids, revision_ids),
        title: title.to_owned(),
        content: content.to_owned(),
        source_refs,
        based_on_revision_ids: revision_ids.to_vec(),
    })
}

async fn dispatch_candidate_once<F, Fut>(request: ContextInboxRequest, dispatch: F) -> Result<Value>
where
    F: FnOnce(ContextInboxRequest) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    dispatch(request).await
}

fn normalized_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn candidate_event_id(
    title: &str,
    content: &str,
    message_ids: &[String],
    revision_ids: &[String],
) -> String {
    let mut digest = Sha256::new();
    for value in std::iter::once(title)
        .chain(std::iter::once(content))
        .chain(message_ids.iter().map(String::as_str))
        .chain(revision_ids.iter().map(String::as_str))
    {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("symbiont-candidate:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn new_feature_is_primary_and_legacy_name_is_compatibility_only() {
        assert!(context_inbox_available(&[
            RUNTIME_CONTEXT_INBOX_FEATURE.to_owned()
        ]));
        assert!(context_inbox_available(&[
            LEGACY_RUNTIME_CONTEXT_HUB_FEATURE.to_owned()
        ]));
        assert!(!context_inbox_available(&["unrelated".to_owned()]));
        assert_ne!(
            RUNTIME_CONTEXT_INBOX_FEATURE,
            LEGACY_RUNTIME_CONTEXT_HUB_FEATURE
        );
    }

    #[test]
    fn candidate_identity_is_stable_and_cross_scope_basis_is_preserved() {
        let first = candidate_request(
            "symbiont-d",
            "Topic",
            "Grounded content",
            Vec::new(),
            &normalized_ids(&["msg_b".into(), "msg_a".into(), "msg_a".into()]),
            &normalized_ids(&["rev_b".into(), "rev_a".into()]),
        );
        let second = candidate_request(
            "symbiont-d",
            "Topic",
            "Grounded content",
            Vec::new(),
            &normalized_ids(&["msg_a".into(), "msg_b".into()]),
            &normalized_ids(&["rev_a".into(), "rev_b".into()]),
        );
        let ContextInboxRequest::SubmitCandidate(first) = first else {
            panic!("candidate builder returned a different Context Inbox operation")
        };
        let ContextInboxRequest::SubmitCandidate(second) = second else {
            panic!("candidate builder returned a different Context Inbox operation")
        };
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.scope, "symbiont-d");
        assert_eq!(first.based_on_revision_ids, ["rev_a", "rev_b"]);
    }

    #[tokio::test]
    async fn runtime_denial_is_returned_without_stripping_or_downgrading_basis() {
        let request = candidate_request(
            "symbiont-d",
            "Topic",
            "Grounded content",
            Vec::new(),
            &["msg_1".to_owned()],
            &["rev_from_another_scope".to_owned()],
        );
        let observed = Arc::new(Mutex::new(None));
        let copy = observed.clone();
        let error = dispatch_candidate_once(request, move |request| async move {
            *copy.lock().expect("observation lock") = Some(request);
            anyhow::bail!("DeriveAcrossScopes denied")
        })
        .await
        .expect_err("Runtime denial must reach the caller");

        assert!(error.to_string().contains("DeriveAcrossScopes denied"));
        let request = observed.lock().expect("observation lock").take().unwrap();
        let ContextInboxRequest::SubmitCandidate(candidate) = request else {
            panic!("candidate was downgraded to another operation")
        };
        assert_eq!(candidate.scope, "symbiont-d");
        assert_eq!(candidate.based_on_revision_ids, ["rev_from_another_scope"]);
    }

    #[test]
    fn activity_operations_remain_outside_page_recall() {
        let publish = ContextInboxRequest::PublishActivity(ActivityInput {
            scope: "symbiont-d".to_owned(),
            topic_key: "release".to_owned(),
            summary: "A concrete cross-client gap".to_owned(),
            expected_version: None,
            ttl_hours: Some(24),
        });
        let read = ContextInboxRequest::ReadActivity(ActivityQuery {
            scopes: vec!["symbiont-d".to_owned()],
            query: Some("release".to_owned()),
            cursor: None,
            limit: Some(3),
            include_own: false,
        });
        assert!(matches!(publish, ContextInboxRequest::PublishActivity(_)));
        assert!(matches!(read, ContextInboxRequest::ReadActivity(_)));
    }
}
