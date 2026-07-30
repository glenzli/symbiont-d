use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use pcp_core::{Projection, ReadPagesRequest};
use pcp_sqlite::SqlitePcpStore;
use serde_json::json;

use super::{ContinuityHost, MessageLinks};
use crate::{
    asset::AssetStore,
    memory::{MemoryRole, MessagePart},
};

const ONE_PIXEL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31, 0, 5,
    0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

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
    let continuity = ContinuityHost::open(store).await.expect("open host");
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
    let continuity = ContinuityHost::open(store).await.expect("open host");
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
                input_revision_ids: user.attachment_revision_ids.clone(),
            },
        )
        .await
        .expect("ingest assistant event");
    assert_eq!(
        continuity
            .latest_assistant_revision()
            .await
            .expect("read reply anchor")
            .as_deref(),
        Some(assistant.page.revision_id.as_str())
    );
    let messages_after_user = continuity
        .recent_messages_after(Some(&user.page.revision_id), 20)
        .await
        .expect("read messages after user");
    assert_eq!(messages_after_user.len(), 1);
    assert_eq!(
        messages_after_user[0].revision_id,
        Some(assistant.page.revision_id.clone())
    );
    let recent_messages = continuity
        .recent_messages(20)
        .await
        .expect("read ordered messages");
    assert_eq!(recent_messages.len(), 2);
    assert_eq!(
        recent_messages[0].revision_id,
        Some(user.page.revision_id.clone())
    );
    assert_eq!(
        recent_messages[1].revision_id,
        Some(assistant.page.revision_id.clone())
    );
    let pages = continuity
        .read(ReadPagesRequest {
            revision_ids: vec![
                user.page.revision_id.clone(),
                assistant.page.revision_id.clone(),
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
        relation.relation_type == "responds_to" && relation.to_revision_id == user.page.revision_id
    }));
    assert!(
        !pages[1]
            .relations
            .iter()
            .any(|relation| relation.relation_type == "derived_from")
    );
    assert!(pages[0].revision.source_refs.is_empty());
    let inputs = &pages[1].revision.provenance[0].input_revision_ids;
    assert_eq!(inputs.len(), 2);
    assert!(inputs.contains(&user.attachment_revision_ids[0]));
    assert!(inputs.contains(&user.page.revision_id));
    assert!(matches!(
        user.entry.parts.get(1),
        Some(MessagePart::Image { .. })
    ));

    let assets = continuity
        .read(ReadPagesRequest {
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
