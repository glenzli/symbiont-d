use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use pcp_client::EmbeddedPcpClient;
use pcp_core::{
    AccessPrincipal, AccessPrincipalType, AccessSession, ConsolidationInput, Projection,
    ReadPagesRequest,
};
use pcp_rpc::{RemotePcpClient, serve_unix};
use pcp_sqlite::SqlitePcpStore;
use pcp_store::PcpStore;
use serde_json::json;

use super::{CONVERSATION_NAMESPACE, ContinuityHost, MessageLinks, PROJECT_NAMESPACE};
use crate::{
    asset::AssetStore,
    memory::{
        MemoryRole, MessageDeliveryState, MessageMetadata, MessagePart, MessageQuoteDraft,
        MessageRunMetadata,
    },
    sensing::{InputRoleSnapshot, SensingPresentation, SensingSource, SensingSourceClass},
    signals::SignalEvent,
};

const ONE_PIXEL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31, 0, 5,
    0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[tokio::test]
async fn continuity_runs_through_the_remote_pcp_api() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-pcp-api-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let owner_id = store.owner_id().to_owned();
    let scopes = vec![
        format!("user:{owner_id}"),
        PROJECT_NAMESPACE.to_owned(),
        CONVERSATION_NAMESPACE.to_owned(),
    ];
    let store: Arc<dyn PcpStore> = store;
    let embedded = EmbeddedPcpClient::shared(
        store,
        AccessSession::full_control(
            AccessPrincipal {
                principal_id: "host:contract-test".to_owned(),
                principal_type: AccessPrincipalType::Host,
                display_name: None,
            },
            "session:contract-test",
            scopes,
        ),
    );
    let socket_path =
        std::path::PathBuf::from("/tmp").join(format!("spd-{}.sock", nonce % 1_000_000_000));
    let server_path = socket_path.clone();
    let server = tokio::spawn(async move { serve_unix(server_path, embedded).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    if server.is_finished() {
        panic!("PCP runtime exited during startup: {:?}", server.await);
    }
    let remote = connect_remote_when_ready(&socket_path).await;

    let continuity = ContinuityHost::open(Arc::new(remote))
        .await
        .expect("open host through remote PcpApi");
    assert_eq!(
        continuity.store().access().principal.principal_id,
        "host:contract-test"
    );

    server.abort();
    let _ = server.await;
    let _ = tokio::fs::remove_file(socket_path).await;
    let _ = tokio::fs::remove_dir_all(root).await;
}

async fn connect_remote_when_ready(socket_path: &std::path::Path) -> RemotePcpClient {
    let mut last_error = None;
    for _ in 0..100 {
        match RemotePcpClient::connect(socket_path).await {
            Ok(client) => return client,
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("PCP runtime did not become ready: {last_error:?}");
}

#[tokio::test]
async fn replied_external_signal_becomes_a_visible_pcp_source_reference() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-external-reference-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let signal = SignalEvent {
        id: "signal-reference-test".to_owned(),
        kind: crate::signals::SignalKind::ExternalInput,
        candidate_id: "candidate-reference-test".to_owned(),
        fingerprint: "fingerprint-reference-test".to_owned(),
        actor: InputRoleSnapshot::ambient("luna", "Luna", "gpt-test", "codex"),
        content: "A newly observed scientific result with enough context to discuss.".to_owned(),
        received_text: "The complete received report.".to_owned(),
        presentation: SensingPresentation::Condensed,
        qualification_note: None,
        title: "A scientific result".to_owned(),
        summary: "A compact result summary".to_owned(),
        sources: vec![SensingSource {
            url: "https://example.test/result".to_owned(),
            detail: "Primary report".to_owned(),
        }],
        source_class: SensingSourceClass::Research,
        event_at: Some("2026-08-10T12:00:00.000Z".to_owned()),
        observed_at: "2026-08-10T12:05:00.000Z".to_owned(),
        review_reason: "Worth presenting as an external input".to_owned(),
        related_signal_ids: Vec::new(),
        promoted_revision_id: None,
        hidden: false,
        dismissed: false,
    };
    let source = continuity
        .ingest_external_signal(&signal)
        .await
        .expect("promote external signal");
    let reply = continuity
        .ingest_message(
            MemoryRole::User,
            "This is worth discussing.",
            Vec::new(),
            None,
            MessageLinks {
                input_revision_ids: vec![source.revision_id.clone()],
                ..MessageLinks::default()
            },
        )
        .await
        .expect("ingest user reply");

    let Some(MessagePart::ExternalInput { input }) = reply.entry.parts.first() else {
        panic!("reply should retain a visible external input reference");
    };
    assert_eq!(input.source_revision_id, source.revision_id);
    assert_eq!(input.actor_name, "Luna");
    assert_eq!(input.title, "A scientific result");
    assert_eq!(input.source_count, 1);

    let pages = continuity
        .read(ReadPagesRequest {
            page_ids: Vec::new(),
            revision_ids: vec![reply.page.revision_id.clone()],
            projections: vec![
                Projection::Facets,
                Projection::Provenance,
                Projection::Relations,
            ],
            max_chars: 8_000,
        })
        .await
        .expect("read reply");
    assert!(
        pages[0].revision.provenance[0]
            .input_revision_ids
            .contains(&source.revision_id)
    );
    assert!(pages[0].relations.iter().any(|relation| {
        relation.relation_type == "references" && relation.to_page_id == source.page_id
    }));

    let recent = continuity
        .recent_messages(10)
        .await
        .expect("read recent conversation");
    assert!(matches!(
        recent[0].parts.first(),
        Some(MessagePart::ExternalInput { .. })
    ));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn context_seed_exposes_the_complete_archive_boundary_and_latest_checkpoint() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-context-seed-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let user = continuity
        .ingest_message(
            MemoryRole::User,
            "Keep this thread connected.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest user event");
    let conversation_scope = continuity.conversation_scope().to_owned();
    let checkpoint = continuity
        .write_model_page(
            Some(&conversation_scope),
            "Active thread: context rollover.",
            Some(json!({"kind": "conversation_checkpoint"})),
            Vec::new(),
            vec![user.page.revision_id.clone()],
            Vec::new(),
            Some("checkpoint-test".to_owned()),
        )
        .await
        .expect("write checkpoint");

    let seed = continuity.context_seed(Some(&user)).await;
    assert!(seed.contains("complete symbiont-d transcript"));
    assert!(seed.contains(&user.page.revision_id));
    assert!(seed.contains(&checkpoint.revision_id));
    assert!(seed.contains("native context is recent only"));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn consolidation_rejects_conflicting_host_stable_identities() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-consolidation-identity-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let project_scope = continuity.project_scope().to_owned();
    let canonical = continuity
        .write_model_page(
            Some(&project_scope),
            "First episode with a shared title.",
            Some(json!({
                "kind": "conversation_episode",
                "stableKey": "reflection.episode.first",
                "episodeId": "episode-first"
            })),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some("consolidation-identity:first".to_owned()),
        )
        .await
        .expect("write canonical episode");
    let distinct = continuity
        .write_model_page(
            Some(&project_scope),
            "Second episode with a shared title.",
            Some(json!({
                "kind": "conversation_episode",
                "stableKey": "reflection.episode.second",
                "episodeId": "episode-second"
            })),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some("consolidation-identity:second".to_owned()),
        )
        .await
        .expect("write distinct episode");

    let error = continuity
        .consolidate_model_pages(
            canonical.page_id.clone(),
            canonical.revision_id,
            vec![ConsolidationInput {
                page_id: distinct.page_id,
                expected_revision_id: distinct.revision_id,
            }],
            "An invalid merge of two stable episodes.".to_owned(),
            Some("test".to_owned()),
        )
        .await
        .expect_err("reject conflicting stable identities");
    assert!(
        format!("{error:#}").contains("conflicting Host identity stableKey"),
        "unexpected consolidation error: {error:#}"
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn quotes_multiple_excerpts_from_one_message_with_one_source_relation() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-quotes-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let source = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "The first idea matters. The second idea changes the decision.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest quoted source");
    let quotes = continuity
        .resolve_message_quotes(vec![
            MessageQuoteDraft {
                source_revision_id: source.page.revision_id.clone(),
                selected_text: "The first idea matters.".to_owned(),
                start_offset: Some(0),
                end_offset: Some(23),
                whole_message: false,
            },
            MessageQuoteDraft {
                source_revision_id: source.page.revision_id.clone(),
                selected_text: "The second idea changes the decision.".to_owned(),
                start_offset: Some(24),
                end_offset: Some(61),
                whole_message: false,
            },
            MessageQuoteDraft {
                source_revision_id: source.page.revision_id.clone(),
                selected_text: "This client preview must not become the quote.".to_owned(),
                start_offset: None,
                end_offset: None,
                whole_message: true,
            },
        ])
        .await
        .expect("resolve source excerpts");
    assert_eq!(quotes.len(), 3);
    assert_eq!(quotes[0].source_role, MemoryRole::Assistant);
    assert_eq!(quotes[0].source_sha256, quotes[1].source_sha256);
    assert_eq!(
        quotes[2].text,
        "The first idea matters. The second idea changes the decision."
    );

    let reply = continuity
        .ingest_message(
            MemoryRole::User,
            "These two passages should be considered together.",
            Vec::new(),
            None,
            MessageLinks {
                quotes,
                ..MessageLinks::default()
            },
        )
        .await
        .expect("ingest quoted reply");
    assert!(matches!(
        reply.entry.parts.first(),
        Some(MessagePart::Quote { .. })
    ));
    assert!(matches!(
        reply.entry.parts.get(1),
        Some(MessagePart::Quote { .. })
    ));

    let pages = continuity
        .read(ReadPagesRequest {
            page_ids: Vec::new(),
            revision_ids: vec![reply.page.revision_id.clone()],
            projections: vec![
                Projection::Facets,
                Projection::Provenance,
                Projection::Relations,
            ],
            max_chars: 16_000,
        })
        .await
        .expect("read quoted reply");
    let quote_relations = pages[0]
        .relations
        .iter()
        .filter(|relation| relation.relation_type == "quotes")
        .collect::<Vec<_>>();
    assert_eq!(quote_relations.len(), 1);
    assert_eq!(quote_relations[0].to_page_id, source.page.page_id);
    assert!(
        quote_relations[0]
            .basis_revision_ids
            .contains(&source.page.revision_id)
    );
    assert!(
        pages[0].revision.provenance[0]
            .input_revision_ids
            .contains(&source.page.revision_id)
    );

    let recent = continuity
        .recent_messages(10)
        .await
        .expect("read quoted conversation");
    assert_eq!(recent.len(), 2);
    assert_eq!(
        recent[1]
            .parts
            .iter()
            .filter(|part| matches!(part, MessagePart::Quote { .. }))
            .count(),
        3
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn keeps_temporal_adjacency_out_of_the_page_graph() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-continuity-order-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");

    let assistant = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "A finished discussion about deployment.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest prior assistant event");
    let user = continuity
        .ingest_message(
            MemoryRole::User,
            "A new and unrelated question about photography.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest unrelated user event");

    let page = continuity
        .read(ReadPagesRequest {
            page_ids: Vec::new(),
            revision_ids: vec![user.page.revision_id.clone()],
            projections: vec![Projection::Relations, Projection::Provenance],
            max_chars: 8_000,
        })
        .await
        .expect("read unrelated user event")
        .remove(0);
    assert!(page.relations.is_empty());
    assert!(page.revision.provenance[0].input_revision_ids.is_empty());

    let recent = continuity
        .recent_messages(10)
        .await
        .expect("read chronological conversation");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].revision_id, Some(assistant.page.revision_id));
    assert_eq!(recent[1].revision_id, Some(user.page.revision_id));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn links_images_user_events_and_assistant_responses() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-continuity-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let assets = AssetStore::open(root.join("assets"))
        .await
        .expect("open assets");
    let image = assets
        .save_image(Some("pixel.png"), ONE_PIXEL_PNG)
        .await
        .expect("save image");

    let user = continuity
        .ingest_message(
            MemoryRole::User,
            "Please inspect this.",
            vec![image],
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest user event");
    let assistant = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "It is a one-pixel image.",
            Vec::new(),
            None,
            MessageLinks {
                responds_to: Some(user.page.revision_id.clone()),
                continues_from: None,
                input_revision_ids: user.attachment_revision_ids.clone(),
                surfaced_hunch_revision_ids: Vec::new(),
                quotes: Vec::new(),
                topic: None,
            },
        )
        .await
        .expect("ingest assistant event");
    let continuation = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "One more detail is worth adding.",
            Vec::new(),
            None,
            MessageLinks {
                responds_to: None,
                continues_from: Some(assistant.page.revision_id.clone()),
                input_revision_ids: Vec::new(),
                surfaced_hunch_revision_ids: Vec::new(),
                quotes: Vec::new(),
                topic: None,
            },
        )
        .await
        .expect("ingest assistant continuation");
    assert_eq!(
        continuity
            .latest_assistant_revision()
            .await
            .expect("read reply anchor")
            .as_deref(),
        Some(continuation.page.revision_id.as_str())
    );
    let messages_after_user = continuity
        .live_messages_after(Some(&user.page.revision_id), 20)
        .await
        .expect("read messages after user");
    assert_eq!(messages_after_user.len(), 2);
    assert_eq!(
        messages_after_user[0].revision_id,
        Some(assistant.page.revision_id.clone())
    );
    let recent_messages = continuity
        .recent_messages(20)
        .await
        .expect("read ordered messages");
    assert_eq!(recent_messages.len(), 3);
    assert_eq!(
        recent_messages[0].revision_id,
        Some(user.page.revision_id.clone())
    );
    assert_eq!(
        recent_messages[1].revision_id,
        Some(assistant.page.revision_id.clone())
    );
    assert_eq!(
        recent_messages[2].revision_id,
        Some(continuation.page.revision_id.clone())
    );
    let selected_messages = continuity
        .messages_by_revision_ids(&[
            continuation.page.revision_id.clone(),
            user.page.revision_id.clone(),
        ])
        .await
        .expect("read selected conversation Revisions");
    assert_eq!(selected_messages.len(), 2);
    assert_eq!(
        selected_messages[0].revision_id,
        Some(user.page.revision_id.clone())
    );
    assert_eq!(
        selected_messages[1].revision_id,
        Some(continuation.page.revision_id.clone())
    );
    assert_eq!(
        recent_messages[0].delivery_state,
        Some(MessageDeliveryState::Delivered)
    );
    let pages = continuity
        .read(ReadPagesRequest {
            page_ids: Vec::new(),
            revision_ids: vec![
                user.page.revision_id.clone(),
                assistant.page.revision_id.clone(),
                continuation.page.revision_id.clone(),
            ],
            projections: vec![
                Projection::Payload,
                Projection::Facets,
                Projection::Sources,
                Projection::Provenance,
                Projection::Relations,
            ],
            max_chars: 8_000,
        })
        .await
        .expect("read events");

    assert!(
        pages[0]
            .relations
            .iter()
            .any(|relation| relation.relation_type == "has_attachment")
    );
    assert!(pages[1].relations.iter().any(|relation| {
        relation.relation_type == "responds_to" && relation.to_page_id == user.page.page_id
    }));
    assert!(
        !pages[1]
            .relations
            .iter()
            .any(|relation| relation.relation_type == "derived_from")
    );
    assert!(pages[2].relations.iter().any(|relation| {
        relation.relation_type == "continues" && relation.to_page_id == assistant.page.page_id
    }));
    assert!(pages[0].revision.source_refs.is_empty());
    let inputs = &pages[1].revision.provenance[0].input_revision_ids;
    assert_eq!(inputs.len(), 2);
    assert!(inputs.contains(&user.attachment_revision_ids[0]));
    assert!(inputs.contains(&user.page.revision_id));
    assert!(
        pages[2].revision.provenance[0]
            .input_revision_ids
            .contains(&assistant.page.revision_id)
    );
    assert!(matches!(
        user.entry.parts.get(1),
        Some(MessagePart::Image { .. })
    ));

    let assets = continuity
        .read(ReadPagesRequest {
            page_ids: Vec::new(),
            revision_ids: user.attachment_revision_ids.clone(),
            projections: vec![Projection::Payload, Projection::Facets, Projection::Sources],
            max_chars: 8_000,
        })
        .await
        .expect("read image asset");
    let asset_revision = &assets[0].revision;
    assert_eq!(asset_revision.source_refs.len(), 1);
    assert!(asset_revision.source_refs[0].metadata.is_none());
    assert!(
        asset_revision
            .facets
            .as_ref()
            .unwrap()
            .get("asset")
            .is_none()
    );
    let descriptor: serde_json::Value =
        serde_json::from_str(&asset_revision.payload.as_ref().unwrap().content)
            .expect("parse canonical image descriptor");
    assert_eq!(descriptor["filename"], "pixel.png");

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn stores_generated_images_as_assistant_page_attachments() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-generated-continuity-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let assets = AssetStore::open(root.join("assets"))
        .await
        .expect("open assets");
    let generated_path = root.join("codex-generated.png");
    tokio::fs::write(&generated_path, ONE_PIXEL_PNG)
        .await
        .expect("write generated fixture");
    let image = assets
        .import_generated_image(
            &generated_path,
            Some(json!({
                "codexItemId": "image-item-1",
                "revisedPrompt": "A one-pixel generated prototype"
            })),
        )
        .await
        .expect("import generated image");

    let assistant = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "Here is the generated prototype.",
            vec![image],
            Some(MessageMetadata {
                runs: vec![MessageRunMetadata {
                    model: "gpt-image-test".to_owned(),
                    display_name: "Image Test".to_owned(),
                    effort: "medium".to_owned(),
                    lane: "conversation".to_owned(),
                    total_tokens: 10,
                    duration_ms: 20,
                }],
                total_tokens: 10,
                duration_ms: 20,
                tool_calls: 1,
                pcp_tool_calls: 0,
                trace_id: Some("trace-image".to_owned()),
                origin: Some("interactive".to_owned()),
            }),
            MessageLinks::default(),
        )
        .await
        .expect("ingest assistant image");

    assert!(matches!(
        assistant.entry.parts.get(1),
        Some(MessagePart::Image { .. })
    ));
    assert_eq!(assistant.attachment_revision_ids.len(), 1);

    let pages = continuity
        .read(ReadPagesRequest {
            page_ids: Vec::new(),
            revision_ids: vec![
                assistant.page.revision_id.clone(),
                assistant.attachment_revision_ids[0].clone(),
            ],
            projections: vec![
                Projection::Payload,
                Projection::Facets,
                Projection::Sources,
                Projection::Provenance,
                Projection::Relations,
            ],
            max_chars: 8_000,
        })
        .await
        .expect("read assistant and generated asset pages");
    assert!(pages[0].relations.iter().any(|relation| {
        relation.relation_type == "has_attachment"
            && relation
                .basis_revision_ids
                .contains(&assistant.attachment_revision_ids[0])
    }));
    assert_eq!(
        pages[1].revision.source_refs[0].source_type,
        "codex_image_generation"
    );
    assert_eq!(
        pages[1].revision.source_refs[0].metadata.as_ref().unwrap()["codexItemId"],
        "image-item-1"
    );
    assert_eq!(
        pages[1].revision.provenance[0].tool_or_model.as_deref(),
        Some("gpt-image-test")
    );
    assert_eq!(pages[1].revision.created_by.actor_id, "codex:symbiont-d");

    let recent_images = continuity
        .recent_image_assets(4)
        .await
        .expect("read recent image assets");
    assert_eq!(recent_images.len(), 1);
    assert_eq!(
        recent_images[0].revision_id,
        assistant.attachment_revision_ids[0]
    );
    assert_eq!(
        recent_images[0].attached_to_revision_id.as_deref(),
        Some(assistant.page.revision_id.as_str())
    );
    assert_eq!(
        recent_images[0].revised_prompt.as_deref(),
        Some("A one-pixel generated prototype")
    );
    assert_eq!(
        continuity
            .attached_image_revision_ids(std::slice::from_ref(&assistant.page.revision_id))
            .await
            .expect("follow message image relation"),
        assistant.attachment_revision_ids
    );

    let recent = continuity
        .recent_messages(10)
        .await
        .expect("reload assistant message");
    assert!(matches!(
        recent[0].parts.get(1),
        Some(MessagePart::Image { .. })
    ));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn marks_unanswered_messages_failed_and_retracts_the_latest_turn() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-retract-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let earlier = continuity
        .ingest_message(
            MemoryRole::User,
            "An older event without explicit response metadata.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest earlier event");
    let user = continuity
        .ingest_message(
            MemoryRole::User,
            "This request did not finish.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest user event");
    let derived = continuity
        .write_model_page(
            None,
            "A provisional note from the failed turn.",
            Some(json!({"kind": "provisional_note"})),
            Vec::new(),
            vec![user.page.revision_id.clone()],
            Vec::new(),
            None,
        )
        .await
        .expect("write derived Page");

    let messages = continuity
        .recent_messages(20)
        .await
        .expect("read failed message");
    assert_eq!(
        messages[0].delivery_state,
        Some(MessageDeliveryState::Delivered)
    );
    assert_eq!(
        messages[1].delivery_state,
        Some(MessageDeliveryState::Failed)
    );

    let result = continuity
        .retract_user_message_and_after(&user.page.revision_id)
        .await
        .expect("retract latest user message");
    assert!(result.message_revision_ids.contains(&user.page.revision_id));
    assert!(result.retracted_revision_ids.contains(&derived.revision_id));
    let remaining = continuity
        .recent_messages(20)
        .await
        .expect("read active messages");
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        remaining[0].revision_id.as_deref(),
        Some(earlier.page.revision_id.as_str())
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn retracting_a_user_message_removes_its_conversation_suffix() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-retract-suffix-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = ContinuityHost::open_embedded_for_test(store)
        .await
        .expect("open host");
    let first_user = continuity
        .ingest_message(
            MemoryRole::User,
            "The original premise.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest first user event");
    let first_assistant = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "The first answer.",
            Vec::new(),
            None,
            MessageLinks {
                responds_to: Some(first_user.page.revision_id.clone()),
                ..MessageLinks::default()
            },
        )
        .await
        .expect("ingest first assistant event");
    let second_user = continuity
        .ingest_message(
            MemoryRole::User,
            "A follow-up based on that answer.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest second user event");
    let second_assistant = continuity
        .ingest_message(
            MemoryRole::Assistant,
            "The second answer.",
            Vec::new(),
            None,
            MessageLinks {
                responds_to: Some(second_user.page.revision_id.clone()),
                ..MessageLinks::default()
            },
        )
        .await
        .expect("ingest second assistant event");

    let result = continuity
        .retract_user_message_and_after(&first_user.page.revision_id)
        .await
        .expect("retract conversation suffix");
    for revision_id in [
        &first_user.page.revision_id,
        &first_assistant.page.revision_id,
        &second_user.page.revision_id,
        &second_assistant.page.revision_id,
    ] {
        assert!(result.message_revision_ids.contains(revision_id));
    }
    assert!(
        continuity
            .recent_messages(20)
            .await
            .expect("read remaining messages")
            .is_empty()
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}
