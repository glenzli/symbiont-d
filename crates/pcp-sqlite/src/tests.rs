use std::time::{SystemTime, UNIX_EPOCH};

use pcp_core::{
    Actor, ActorType, CreateScopeRequest, LifecycleStatus, LinkPagesRequest, PagePayload,
    Projection, ProvenanceEvent, ReadPagesRequest, RevisePageRequest, SearchFilters, SearchMode,
    SearchPagesRequest, SourceRef, WritePageRequest, WriteSummaryRequest,
};

use super::SqlitePcpStore;

#[tokio::test]
async fn stores_searches_revises_and_links_pages() {
    let root = std::env::temp_dir().join(format!(
        "pcp-sqlite-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let path = root.join("pcp.sqlite3");
    let store = SqlitePcpStore::open(path).await.expect("open store");
    let owner_id = store.owner_id().to_owned();
    let namespace = "conversation:test".to_owned();
    store
        .create_scope(CreateScopeRequest {
            owner_id: owner_id.clone(),
            namespace: namespace.clone(),
            scope_type: "conversation".to_owned(),
            display_name: "Test conversation".to_owned(),
            description: None,
            parent_namespace: None,
            visibility: "private".to_owned(),
        })
        .await
        .expect("create scope");

    let actor = Actor {
        actor_type: ActorType::User,
        actor_id: "user:test".to_owned(),
    };
    let first = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "A compactness argument using finite products.",
                "event:first",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("write first page");
    let duplicate = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "A compactness argument using finite products.",
                "event:first",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("repeat first write");
    assert_eq!(duplicate.page_id, first.page_id);
    assert!(!duplicate.created);

    let search = store
        .search_pages(SearchPagesRequest {
            query: "compactness products".to_owned(),
            scopes: vec![namespace.clone()],
            mode: SearchMode::Text,
            filters: SearchFilters::default(),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("search page");
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.hits[0].revision_id, first.revision_id);

    let revised = store
        .revise_page(
            RevisePageRequest {
                page_id: first.page_id.clone(),
                expected_revision_id: first.revision_id.clone(),
                created_by: actor.clone(),
                lifecycle_status: LifecycleStatus::Active,
                observed_at: None,
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: "The finite-product compactness argument is now verified.".to_owned(),
                }),
                source_refs: vec![SourceRef {
                    source_type: "test_file".to_owned(),
                    uri: "file:///tmp/pcp-source.md".to_owned(),
                    locator: Some("L1-L3".to_owned()),
                    metadata: None,
                }],
                facets: None,
                provenance: vec![ProvenanceEvent {
                    operation: "revise".to_owned(),
                    actor: actor.clone(),
                    timestamp: "2026-07-29T00:00:00Z".to_owned(),
                    input_revision_ids: vec![first.revision_id.clone(), first.revision_id.clone()],
                    tool_or_model: Some("test".to_owned()),
                }],
                idempotency_key: Some("revision:first".to_owned()),
            },
            vec![namespace.clone()],
        )
        .await
        .expect("revise page");
    assert_ne!(revised.revision_id, first.revision_id);

    let derived_from_source = store
        .search_pages(SearchPagesRequest {
            query: first.revision_id.clone(),
            scopes: vec![namespace.clone()],
            mode: SearchMode::Graph,
            filters: SearchFilters {
                relation_types: vec!["derived_from".to_owned()],
                ..SearchFilters::default()
            },
            limit: 10,
            cursor: None,
        })
        .await
        .expect("traverse provenance from source");
    assert_eq!(derived_from_source.hits.len(), 1);
    assert_eq!(derived_from_source.hits[0].revision_id, revised.revision_id);

    let source_from_derived = store
        .search_pages(SearchPagesRequest {
            query: revised.revision_id.clone(),
            scopes: vec![namespace.clone()],
            mode: SearchMode::Graph,
            filters: SearchFilters {
                relation_types: vec!["derived_from".to_owned()],
                ..SearchFilters::default()
            },
            limit: 10,
            cursor: None,
        })
        .await
        .expect("traverse provenance from derived revision");
    assert_eq!(source_from_derived.hits.len(), 1);
    assert_eq!(source_from_derived.hits[0].revision_id, first.revision_id);

    let second = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "A related open question.",
                "event:second",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("write second page");
    store
        .link_pages(
            LinkPagesRequest {
                from_revision_id: second.revision_id.clone(),
                relation_type: "depends_on".to_owned(),
                to_revision_id: revised.revision_id.clone(),
                created_by: actor.clone(),
                idempotency_key: Some("link:first".to_owned()),
            },
            vec![namespace.clone()],
        )
        .await
        .expect("link pages");

    let graph = store
        .search_pages(SearchPagesRequest {
            query: second.revision_id,
            scopes: vec![namespace.clone()],
            mode: SearchMode::Graph,
            filters: SearchFilters::default(),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("search graph");
    assert_eq!(graph.hits.len(), 1);
    assert_eq!(graph.hits[0].revision_id, revised.revision_id);

    let read = store
        .read_pages(
            ReadPagesRequest {
                revision_ids: vec![revised.revision_id.clone()],
                projections: vec![
                    Projection::Payload,
                    Projection::Relations,
                    Projection::History,
                ],
                max_chars: 10_000,
            },
            vec![namespace.clone()],
        )
        .await
        .expect("read revised page");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].history.len(), 2);
    assert_eq!(read[0].relations.len(), 1);
    assert!(read[0].revision.source_refs.is_empty());
    assert!(read[0].revision.provenance.is_empty());
    let lean_json = serde_json::to_value(&read[0]).expect("serialize lean projection");
    assert!(lean_json["revision"].get("sourceRefs").is_none());
    assert!(lean_json["revision"].get("provenance").is_none());

    let traced = store
        .read_pages(
            ReadPagesRequest {
                revision_ids: vec![revised.revision_id],
                projections: vec![Projection::Sources, Projection::Provenance],
                max_chars: 10_000,
            },
            vec![namespace.clone()],
        )
        .await
        .expect("read source and provenance");
    assert_eq!(traced[0].revision.source_refs.len(), 1);
    assert_eq!(traced[0].revision.provenance.len(), 1);
    assert_eq!(traced[0].revision.provenance[0].input_revision_ids.len(), 1);

    let mut invalid = write_request(
        &owner_id,
        &namespace,
        actor.clone(),
        "This Page cites a missing revision.",
        "event:invalid-provenance",
    );
    invalid.provenance = vec![ProvenanceEvent {
        operation: "derive".to_owned(),
        actor,
        timestamp: "2026-07-29T00:00:00Z".to_owned(),
        input_revision_ids: vec!["rev_missing".to_owned()],
        tool_or_model: Some("test".to_owned()),
    }];
    let error = store
        .write_page(invalid, vec![namespace])
        .await
        .expect_err("reject missing provenance input");
    assert!(error.to_string().contains("find PCP revision rev_missing"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn backfills_the_provenance_graph_index() {
    let root = std::env::temp_dir().join(format!(
        "pcp-sqlite-backfill-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let path = root.join("pcp.sqlite3");
    let store = SqlitePcpStore::open(path.clone())
        .await
        .expect("open store");
    let owner_id = store.owner_id().to_owned();
    let namespace = "conversation:backfill".to_owned();
    store
        .create_scope(CreateScopeRequest {
            owner_id: owner_id.clone(),
            namespace: namespace.clone(),
            scope_type: "conversation".to_owned(),
            display_name: "Backfill conversation".to_owned(),
            description: None,
            parent_namespace: None,
            visibility: "private".to_owned(),
        })
        .await
        .expect("create scope");
    let actor = Actor {
        actor_type: ActorType::Model,
        actor_id: "model:test".to_owned(),
    };
    let source = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "Source Page",
                "backfill:source",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("write source");
    let mut derived_request = write_request(
        &owner_id,
        &namespace,
        actor.clone(),
        "Derived Page",
        "backfill:derived",
    );
    derived_request.provenance = vec![ProvenanceEvent {
        operation: "derive".to_owned(),
        actor,
        timestamp: "2026-07-29T00:00:00Z".to_owned(),
        input_revision_ids: vec![source.revision_id.clone()],
        tool_or_model: Some("test".to_owned()),
    }];
    let derived = store
        .write_page(derived_request, vec![namespace.clone()])
        .await
        .expect("write derived page");
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("open raw database");
    connection
        .execute_batch(
            "
            DROP TABLE pcp_provenance_inputs;
            DELETE FROM pcp_metadata WHERE key = 'provenance_input_index_version';
            ",
        )
        .expect("remove derived index");
    drop(connection);

    let reopened = SqlitePcpStore::open(path).await.expect("reopen store");
    let graph = reopened
        .search_pages(SearchPagesRequest {
            query: source.revision_id,
            scopes: vec![namespace],
            mode: SearchMode::Graph,
            filters: SearchFilters {
                relation_types: vec!["derived_from".to_owned()],
                ..SearchFilters::default()
            },
            limit: 10,
            cursor: None,
        })
        .await
        .expect("search backfilled graph");
    assert_eq!(graph.hits.len(), 1);
    assert_eq!(graph.hits[0].revision_id, derived.revision_id);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn writes_searches_reads_and_revises_sparse_summaries() {
    let root = std::env::temp_dir().join(format!(
        "pcp-summary-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let store = SqlitePcpStore::open(root.join("pcp.sqlite3"))
        .await
        .expect("open store");
    let owner_id = store.owner_id().to_owned();
    let namespace = "conversation:summary".to_owned();
    store
        .create_scope(CreateScopeRequest {
            owner_id: owner_id.clone(),
            namespace: namespace.clone(),
            scope_type: "conversation".to_owned(),
            display_name: "Summary conversation".to_owned(),
            description: None,
            parent_namespace: None,
            visibility: "private".to_owned(),
        })
        .await
        .expect("create scope");
    let actor = Actor {
        actor_type: ActorType::Model,
        actor_id: "model:summary".to_owned(),
    };
    let page = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "A long discussion whose routing value is not captured by its opening words.",
                "summary:page",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("write page");
    assert_eq!(
        store
            .next_summary_candidate(vec![namespace.clone()], 10)
            .await
            .expect("find summary candidate")
            .as_deref(),
        Some(page.revision_id.as_str())
    );

    let summary = store
        .write_summary(
            WriteSummaryRequest {
                target_revision_id: page.revision_id.clone(),
                expected_summary_revision_id: None,
                content:
                    "Resource-pool ownership and cancellation semantics for background workers."
                        .to_owned(),
                created_by: actor.clone(),
                tool_or_model: Some("small-model".to_owned()),
                provenance: Vec::new(),
                idempotency_key: Some("summary:first".to_owned()),
            },
            vec![namespace.clone()],
        )
        .await
        .expect("write summary");
    let duplicate = store
        .write_summary(
            WriteSummaryRequest {
                target_revision_id: page.revision_id.clone(),
                expected_summary_revision_id: None,
                content: "Ignored duplicate content.".to_owned(),
                created_by: actor.clone(),
                tool_or_model: Some("small-model".to_owned()),
                provenance: Vec::new(),
                idempotency_key: Some("summary:first".to_owned()),
            },
            vec![namespace.clone()],
        )
        .await
        .expect("repeat summary write");
    assert_eq!(duplicate.summary_revision_id, summary.summary_revision_id);
    assert!(!duplicate.created);

    let search = store
        .search_pages(SearchPagesRequest {
            query: "resource pool cancellation".to_owned(),
            scopes: vec![namespace.clone()],
            mode: SearchMode::Summary,
            filters: SearchFilters::default(),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("search summaries");
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.hits[0].revision_id, page.revision_id);
    assert_eq!(search.hits[0].matched_projection, "summary");
    assert!(
        search.hits[0]
            .available_projections
            .contains(&Projection::Summary)
    );
    let browsed = store
        .search_pages(SearchPagesRequest {
            query: String::new(),
            scopes: vec![namespace.clone()],
            mode: SearchMode::Summary,
            filters: SearchFilters::default(),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("browse summary index");
    assert_eq!(browsed.hits[0].revision_id, page.revision_id);

    let read = store
        .read_pages(
            ReadPagesRequest {
                revision_ids: vec![page.revision_id.clone()],
                projections: vec![Projection::Summary],
                max_chars: 10_000,
            },
            vec![namespace.clone()],
        )
        .await
        .expect("read summary projection");
    assert!(read[0].revision.payload.is_none());
    let projected = read[0].summary.as_ref().expect("summary projection");
    assert_eq!(projected.summary_revision_id, summary.summary_revision_id);
    assert_eq!(
        projected.provenance[0].input_revision_ids,
        vec![page.revision_id.clone()]
    );

    let revised = store
        .write_summary(
            WriteSummaryRequest {
                target_revision_id: page.revision_id.clone(),
                expected_summary_revision_id: Some(summary.summary_revision_id.clone()),
                content:
                    "Background worker ownership, cancellation, and terminal publication semantics."
                        .to_owned(),
                created_by: actor.clone(),
                tool_or_model: Some("small-model".to_owned()),
                provenance: Vec::new(),
                idempotency_key: Some("summary:revision".to_owned()),
            },
            vec![namespace.clone()],
        )
        .await
        .expect("revise summary");
    assert_ne!(revised.summary_revision_id, summary.summary_revision_id);
    assert!(
        store
            .next_summary_candidate(vec![namespace.clone()], 10)
            .await
            .expect("summary removes candidate")
            .is_none()
    );

    let conflict = store
        .write_summary(
            WriteSummaryRequest {
                target_revision_id: page.revision_id,
                expected_summary_revision_id: Some(summary.summary_revision_id),
                content: "Stale update.".to_owned(),
                created_by: actor,
                tool_or_model: Some("small-model".to_owned()),
                provenance: Vec::new(),
                idempotency_key: Some("summary:stale".to_owned()),
            },
            vec![namespace],
        )
        .await
        .expect_err("reject stale summary update");
    assert!(conflict.to_string().contains("summary conflict"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn rejects_cycles_in_the_derivation_subgraph() {
    let root = std::env::temp_dir().join(format!(
        "pcp-dag-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let store = SqlitePcpStore::open(root.join("pcp.sqlite3"))
        .await
        .expect("open store");
    let owner_id = store.owner_id().to_owned();
    let namespace = "project:dag".to_owned();
    store
        .create_scope(CreateScopeRequest {
            owner_id: owner_id.clone(),
            namespace: namespace.clone(),
            scope_type: "project".to_owned(),
            display_name: "DAG project".to_owned(),
            description: None,
            parent_namespace: None,
            visibility: "private".to_owned(),
        })
        .await
        .expect("create scope");
    let actor = Actor {
        actor_type: ActorType::Model,
        actor_id: "model:dag".to_owned(),
    };
    let first = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "First Page",
                "dag:first",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("write first");
    let second = store
        .write_page(
            write_request(
                &owner_id,
                &namespace,
                actor.clone(),
                "Second Page",
                "dag:second",
            ),
            vec![namespace.clone()],
        )
        .await
        .expect("write second");
    store
        .link_pages(
            LinkPagesRequest {
                from_revision_id: second.revision_id.clone(),
                relation_type: "summarizes".to_owned(),
                to_revision_id: first.revision_id.clone(),
                created_by: actor.clone(),
                idempotency_key: Some("dag:forward".to_owned()),
            },
            vec![namespace.clone()],
        )
        .await
        .expect("create forward edge");

    let cycle = store
        .link_pages(
            LinkPagesRequest {
                from_revision_id: first.revision_id,
                relation_type: "derived_from".to_owned(),
                to_revision_id: second.revision_id,
                created_by: actor,
                idempotency_key: Some("dag:cycle".to_owned()),
            },
            vec![namespace],
        )
        .await
        .expect_err("reject derivation cycle");
    assert!(cycle.to_string().contains("cycle"));

    let _ = std::fs::remove_dir_all(root);
}

fn write_request(
    owner_id: &str,
    namespace: &str,
    actor: Actor,
    content: &str,
    idempotency_key: &str,
) -> WritePageRequest {
    WritePageRequest {
        owner_id: owner_id.to_owned(),
        namespace: namespace.to_owned(),
        visibility: "private".to_owned(),
        lifecycle_status: LifecycleStatus::Active,
        created_by: actor,
        observed_at: None,
        valid_from: None,
        valid_to: None,
        payload: Some(PagePayload {
            media_type: "text/markdown".to_owned(),
            content: content.to_owned(),
        }),
        source_refs: Vec::new(),
        facets: None,
        provenance: Vec::new(),
        initial_relations: Vec::new(),
        idempotency_key: Some(idempotency_key.to_owned()),
    }
}
