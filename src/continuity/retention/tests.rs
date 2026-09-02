use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pcp_client::{EmbeddedPcpClient, PcpApi, PcpTenantApi};
use pcp_core::{
    ContextDetail, ContextPackEntry, CreateScopeRequest, IntentEffort, QueryContextResponse,
    QueryVisibility, RepairPageRequest,
};
use pcp_rpc::{RemotePcpClient, RunningRuntimeEndpoint, RuntimeQueryService};
use pcp_sqlite::SqlitePcpStore;
use pcp_store::PcpStore;

use super::*;
use crate::{
    memory::MessagePart,
    transcript::{TranscriptMessageLinks, TranscriptStore},
};

struct QueryFixture {
    available: Arc<AtomicBool>,
    empty_index: bool,
}
#[async_trait::async_trait]
impl RuntimeQueryService for QueryFixture {
    async fn semantic_search(
        &self,
        client: &dyn PcpTenantApi,
        request: QueryContextRequest,
    ) -> Result<QueryContextResponse> {
        anyhow::ensure!(
            self.available.load(Ordering::SeqCst),
            "query provider unavailable"
        );
        let found = client
            .search_pages(pcp_core::SearchPagesRequest {
                query: String::new(),
                scopes: request.scopes.clone(),
                mode: pcp_core::SearchMode::Temporal,
                term_match: Default::default(),
                projections: vec![Projection::Payload],
                filters: Default::default(),
                limit: 8,
                cursor: None,
            })
            .await?;
        let entries = if self.empty_index {
            vec![]
        } else {
            found
                .hits
                .into_iter()
                .enumerate()
                .map(|(rank, hit)| ContextPackEntry {
                    rank,
                    anchor_rank: rank,
                    page_id: hit.page_id,
                    revision_id: hit.revision_id,
                    namespace: hit.namespace,
                    kind: hit.kind,
                    matched_by: "semantic".to_owned(),
                    matched_projection: "payload".to_owned(),
                    semantic_score: Some(0.9),
                    structural_boost: None,
                    structural_relations: vec![],
                    intent_reason: None,
                    detail: ContextDetail::Payload,
                    relation: None,
                    source_projection_truncated: false,
                    content: Some(hit.snippet),
                    source_span: None,
                    provenance_revision_ids: vec![],
                    validity: None,
                })
                .collect()
        };
        Ok(QueryContextResponse {
            scopes: request.scopes,
            visibility: QueryVisibility::Scoped,
            result_limit: 8,
            context_budget_chars: 16000,
            anchor_count: entries.len(),
            related_count: 0,
            semantic_indexed_count: Some(entries.len()),
            semantic_embedded_count: Some(0),
            semantic_model_calls: Some(0),
            intent_match: None,
            entries,
        })
    }
    async fn match_intent(
        &self,
        client: &dyn PcpTenantApi,
        request: QueryContextRequest,
        _effort: IntentEffort,
    ) -> Result<QueryContextResponse> {
        self.semantic_search(client, request).await
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    host: ContinuityHost,
    transcript: Arc<TranscriptStore>,
    api: Arc<dyn PcpApi>,
    available: Arc<AtomicBool>,
    _endpoint: RunningRuntimeEndpoint,
}
impl Fixture {
    async fn new(empty_index: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("sret-")
            .tempdir_in("/tmp")
            .unwrap();
        let store: Arc<dyn PcpStore> = Arc::new(
            SqlitePcpStore::open(root.path().join("pcp.sqlite3"))
                .await
                .unwrap(),
        );
        let access = ContinuityHost::access_session("host:symbiont-d");
        store
            .create_scope(
                &access,
                CreateScopeRequest {
                    namespace: "symbiont-d".to_owned(),
                    display_name: "test".to_owned(),
                    description: None,
                    parent_namespace: None,
                },
            )
            .await
            .unwrap();
        let api = EmbeddedPcpClient::shared(store, access);
        let available = Arc::new(AtomicBool::new(true));
        let socket = root.path().join("p.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let endpoint = RunningRuntimeEndpoint::from_bound_listener_with_query(
            &socket,
            listener,
            api.clone(),
            Some(Arc::new(QueryFixture {
                available: available.clone(),
                empty_index,
            })),
        );
        let client = Arc::new(RemotePcpClient::connect(&socket).await.unwrap());
        let transcript = Arc::new(
            TranscriptStore::open(root.path().join("transcript.sqlite3"), None)
                .await
                .unwrap()
                .0,
        );
        let host = ContinuityHost::open(client, transcript.clone())
            .await
            .unwrap();
        Self {
            _root: root,
            host,
            transcript,
            api,
            available,
            _endpoint: endpoint,
        }
    }
    async fn source(&self, at: &str, role: MemoryRole, content: &str) -> String {
        self.transcript
            .append(
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
                },
                TranscriptMessageLinks::default(),
            )
            .await
            .unwrap()
            .message_id
    }
    async fn count(&self) -> usize {
        self.api
            .page_count(vec!["symbiont-d".to_owned()])
            .await
            .unwrap() as usize
    }
}
fn proposal(source: &str, content: &str) -> Proposal {
    Proposal {
        kind: Some("user_constraint".to_owned()),
        content: content.to_owned(),
        source_message_ids: vec![source.to_owned()],
        based_on_revision_ids: vec![],
    }
}
fn decision(packet: &Value, disposition: Disposition, related: Vec<String>) -> RetentionReview {
    RetentionReview {token:packet["reviewToken"].as_str().unwrap().to_owned(),disposition,rationale:"The exact source supports this decision; no assistant-added requirements are being promoted.".to_owned(),attribution:Attribution::UserStatement,related_revision_ids:related}
}

#[tokio::test]
async fn unavailable_query_defers_without_writing_and_survives_restart() {
    let f = Fixture::new(false).await;
    let source = f
        .source(
            "2026-08-05T10:09:34Z",
            MemoryRole::User,
            "关注 Qwen 本地部署",
        )
        .await;
    f.available.store(false, Ordering::SeqCst);
    let p = proposal(&source, "用户希望了解 Qwen 本地部署。");
    let result = f.host.retain_page(p.clone(), None).await.unwrap();
    assert_eq!(result["status"], "deferred");
    assert_eq!(f.count().await, 0);
    let reopened = RetentionQueue::open(RetentionQueue::path_for(
        f.transcript.path(),
        f.api.identity_id(),
    ))
    .await
    .unwrap();
    assert_eq!(reopened.state.lock().await.proposals.len(), 1);
    let other_store = RetentionQueue::open(RetentionQueue::path_for(
        f.transcript.path(),
        "idn_another_store",
    ))
    .await
    .unwrap();
    assert!(other_store.state.lock().await.proposals.is_empty());
    f.available.store(true, Ordering::SeqCst);
    let packet = f.host.retain_page(p, None).await.unwrap();
    assert_eq!(packet["status"], "review_required");
    assert_eq!(f.count().await, 0);
}

#[tokio::test]
async fn historical_source_time_and_assistant_attribution_are_preserved() {
    let f = Fixture::new(false).await;
    let source = f
        .source(
            "2026-08-05T10:09:34.031Z",
            MemoryRole::User,
            "等到 MinMax H3 本地部署流畅时提醒我。",
        )
        .await;
    let p = proposal(&source, "用户当时希望在 MinMax H3 本地部署流畅时获得提醒。");
    let packet = f.host.retain_page(p.clone(), None).await.unwrap();
    assert_eq!(packet["sourceEvidence"][0]["role"], "user");
    let saved = f
        .host
        .retain_page(
            p.clone(),
            Some(decision(&packet, Disposition::NewSubject, vec![])),
        )
        .await
        .unwrap();
    assert_eq!(saved["status"], "written");
    assert_eq!(saved["observedAt"], "2026-08-05T10:09:34.031Z");
    let page = f
        .api
        .read_pages(ReadPagesRequest {
            page_ids: vec![saved["pageId"].as_str().unwrap().to_owned()],
            revision_ids: vec![],
            projections: vec![Projection::Payload, Projection::Facets, Projection::Sources],
            max_chars: 16000,
        })
        .await
        .unwrap()
        .remove(0);
    let content = &page.revision.payload.as_ref().unwrap().content;
    assert!(content.contains("2026-08-05"));
    assert!(content.contains("不表示该意向"));
    assert!(!content.contains("稳定"));
    assert_eq!(page.revision.source_refs.len(), 1);
    let reused = f.host.retain_page(p, None).await.unwrap();
    assert_eq!(reused["pageId"], saved["pageId"]);
    assert_eq!(reused["created"], false);
    assert_eq!(reused["reusedReceipt"], true);
    assert_eq!(f.count().await, 1);
    let assistant = f
        .source(&timestamp(), MemoryRole::Assistant, "我建议稳定且可复现。")
        .await;
    let p = proposal(&assistant, "用户要求稳定且可复现。");
    let packet = f.host.retain_page(p.clone(), None).await.unwrap();
    assert!(
        f.host
            .retain_page(p, Some(decision(&packet, Disposition::NewSubject, vec![])))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn foreground_receipt_is_visible_to_background_even_before_vector_index_catches_up() {
    let f = Fixture::new(true).await;
    let source = f
        .source(
            &timestamp(),
            MemoryRole::User,
            "AI 会让内容膨胀，不能让体量代替价值。",
        )
        .await;
    let first = proposal(
        &source,
        "用户认为 AI 时代的内容保留需要价值理由，不能由体量代替。",
    );
    let later = proposal(
        &source,
        "跨时段问题：AI 造成抽象膨胀，保留资格不能由总量决定。",
    );
    let background_packet = f.host.retain_page(later.clone(), None).await.unwrap();
    let packet = f.host.retain_page(first.clone(), None).await.unwrap();
    let saved = f
        .host
        .retain_page(
            first,
            Some(decision(&packet, Disposition::NewSubject, vec![])),
        )
        .await
        .unwrap();
    let refreshed = f
        .host
        .retain_page(
            later.clone(),
            Some(decision(
                &background_packet,
                Disposition::NewSubject,
                vec![],
            )),
        )
        .await
        .unwrap();
    assert_eq!(refreshed["status"], "review_required");
    assert_eq!(
        refreshed["currentPages"][0]["revision"]["revisionId"],
        saved["revisionId"]
    );
    let covered = f
        .host
        .retain_page(
            later,
            Some(decision(
                &refreshed,
                Disposition::Covered,
                vec![saved["revisionId"].as_str().unwrap().to_owned()],
            )),
        )
        .await
        .unwrap();
    assert_eq!(covered["status"], "covered");
    assert_eq!(f.count().await, 1);
}

#[tokio::test]
async fn current_revision_repairs_invalidate_issued_reviews() {
    let f = Fixture::new(false).await;
    let source = f
        .source(
            "2026-08-05T10:09:34Z",
            MemoryRole::User,
            "本地部署流畅时提醒我。",
        )
        .await;
    let old = f
        .api
        .ingest_page(IngestPageRequest {
            namespace: "symbiont-d".to_owned(),
            kind: "open_loop".to_owned(),
            observed_at: Some("2026-08-05T10:09:34Z".to_owned()),
            source_span: None,
            payload: Some(PagePayload {
                media_type: "text/markdown".to_owned(),
                content: "用户希望部署稳定且可复现。".to_owned(),
            }),
            source_refs: vec![],
            based_on_revision_ids: vec![],
            facets: None,
            external_event_id: None,
        })
        .await
        .unwrap();
    let p = proposal(&source, "用户希望部署稳定且可复现。");
    let packet = f.host.retain_page(p.clone(), None).await.unwrap();
    let repaired = f
        .api
        .repair_page(RepairPageRequest {
            page_id: old.page_id.clone(),
            expected_revision_id: old.revision_id,
            payload: Some(PagePayload {
                media_type: "text/markdown".to_owned(),
                content: "提出于 2026-08-05：用户只要求本地部署流畅；稳定可复现是助手添加。"
                    .to_owned(),
            }),
            source_refs: vec![],
            facets: None,
            based_on_revision_ids: vec![],
            reason: "Restore user/assistant attribution".to_owned(),
            tool_or_model: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    let refreshed = f
        .host
        .retain_page(p, Some(decision(&packet, Disposition::NewSubject, vec![])))
        .await
        .unwrap();
    assert_eq!(refreshed["status"], "review_required");
    assert_ne!(refreshed["reviewToken"], packet["reviewToken"]);
    assert_eq!(
        refreshed["currentPages"][0]["revision"]["revisionId"],
        repaired.revision_id
    );
    assert_eq!(f.count().await, 1);
}

#[tokio::test]
async fn unavailable_query_after_review_and_retracted_sources_never_commit() {
    let f = Fixture::new(false).await;
    let source = f
        .source(
            &timestamp(),
            MemoryRole::User,
            "This is a durable decision.",
        )
        .await;
    let p = proposal(&source, "The user made a durable decision.");
    let packet = f.host.retain_page(p.clone(), None).await.unwrap();
    f.available.store(false, Ordering::SeqCst);
    assert_eq!(
        f.host
            .retain_page(
                p.clone(),
                Some(decision(&packet, Disposition::NewSubject, vec![]))
            )
            .await
            .unwrap()["status"],
        "deferred"
    );
    f.available.store(true, Ordering::SeqCst);
    f.transcript.retract_from(&source).await.unwrap();
    assert_eq!(
        f.host.retain_page(p, None).await.unwrap()["status"],
        "deferred"
    );
    assert_eq!(f.count().await, 0);
}

#[tokio::test]
async fn genuine_addition_preserves_current_revision_provenance() {
    let f = Fixture::new(false).await;
    let source = f
        .source(&timestamp(), MemoryRole::User, "保留内容需要说明价值。")
        .await;
    let first = proposal(&source, "用户要求内容保留有价值依据。");
    let packet = f.host.retain_page(first.clone(), None).await.unwrap();
    let saved = f
        .host
        .retain_page(
            first,
            Some(decision(&packet, Disposition::NewSubject, vec![])),
        )
        .await
        .unwrap();
    let revision = saved["revisionId"].as_str().unwrap().to_owned();
    let fresh = f
        .source(
            &timestamp(),
            MemoryRole::User,
            "再增加一项明确规则：标明适用范围以及撤回条件。",
        )
        .await;
    let addition = proposal(
        &fresh,
        "新增规则：记录的保留依据还应注明适用范围和撤回条件。",
    );
    let packet = f.host.retain_page(addition.clone(), None).await.unwrap();
    let saved = f
        .host
        .retain_page(
            addition,
            Some(decision(
                &packet,
                Disposition::Addition,
                vec![revision.clone()],
            )),
        )
        .await
        .unwrap();
    assert_eq!(saved["status"], "written");
    assert_eq!(f.count().await, 2);
    let page = f
        .api
        .read_pages(ReadPagesRequest {
            page_ids: vec![saved["pageId"].as_str().unwrap().to_owned()],
            revision_ids: vec![],
            projections: vec![Projection::Provenance, Projection::Sources],
            max_chars: 16000,
        })
        .await
        .unwrap()
        .remove(0);
    assert!(
        page.revision
            .provenance
            .iter()
            .any(|event| event.input_revision_ids.contains(&revision))
    );
    assert_eq!(page.revision.source_refs.len(), 1);
    assert!(
        f.host.retention_snapshot().await["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn background_retry_waits_for_query_recovery_and_can_discard_bad_proposal() {
    let f = Fixture::new(false).await;
    let source = f
        .source(
            "2026-08-05T10:09:34Z",
            MemoryRole::User,
            "本地部署流畅时提醒我。",
        )
        .await;
    let p = proposal(&source, "用户要求稳定且可复现。");
    f.available.store(false, Ordering::SeqCst);
    f.host.retain_page(p.clone(), None).await.unwrap();
    for record in f.host.retention.state.lock().await.proposals.values_mut() {
        record.retry_after = "2000-01-01T00:00:00.000Z".to_owned();
    }
    assert!(f.host.retention_retry_bundle().await.unwrap().is_none());
    f.available.store(true, Ordering::SeqCst);
    for record in f.host.retention.state.lock().await.proposals.values_mut() {
        record.retry_after = "2000-01-01T00:00:00.000Z".to_owned();
    }
    let bundle = f.host.retention_retry_bundle().await.unwrap().unwrap();
    assert!(bundle.contains("NOT new user statements"));
    assert!(bundle.contains(&source));
    let packet = f.host.retain_page(p.clone(), None).await.unwrap();
    let discarded = f
        .host
        .retain_page(p, Some(decision(&packet, Disposition::Discard, vec![])))
        .await
        .unwrap();
    assert_eq!(discarded["status"], "discarded");
    assert!(
        f.host.retention_snapshot().await["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        f.host
            .retention
            .state
            .lock()
            .await
            .proposals
            .values()
            .all(|record| record.review.is_none())
    );
    assert_eq!(f.count().await, 0);
}

#[tokio::test]
async fn source_time_compares_instants_not_offset_strings() {
    let f = Fixture::new(false).await;
    let earlier = f
        .source("2026-08-05T20:00:00+08:00", MemoryRole::User, "较早来源")
        .await;
    let later = f
        .source("2026-08-05T13:00:00Z", MemoryRole::User, "较晚来源")
        .await;
    let sources = f.transcript.by_ids(&[earlier, later]).await.unwrap();
    assert_eq!(
        source_time(&sources, &[]).unwrap().0,
        "2026-08-05T13:00:00Z"
    );
}
