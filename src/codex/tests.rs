use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    client::{ChatInput, extract_final_agent_message, multimodal_input_items},
    tools::SymbiontTools,
};
use crate::{
    continuity::ContinuityHost,
    profile::{CalibrationMode, ProfileStore, SetupStatus},
};
use pcp_sqlite::SqlitePcpStore;
use serde_json::json;

#[test]
fn dynamic_tools_expose_host_and_pcp_namespaces() {
    let specs = SymbiontTools::specifications();
    assert_eq!(specs[0]["type"], "namespace");
    assert_eq!(specs[0]["name"], "symbiont");
    assert_eq!(specs[0]["tools"][0]["name"], "complete_orientation");
    assert_eq!(specs[0]["tools"][1]["name"], "escalate");
    assert_eq!(specs[1]["name"], "pcp");
    assert_eq!(specs[1]["tools"][0]["name"], "describe");
    assert_eq!(specs[1]["tools"][2]["name"], "search_pages");
    assert_eq!(specs[1]["tools"][3]["name"], "read_pages");
    assert_eq!(specs[1]["tools"][4]["name"], "write_page");
}

#[test]
fn extracts_the_last_agent_message_from_a_completed_turn() {
    let params = json!({
        "turn": {
            "items": [
                { "type": "agentMessage", "id": "first", "text": "draft" },
                { "type": "agentMessage", "id": "final", "text": "finished" }
            ]
        }
    });

    assert_eq!(
        extract_final_agent_message(&params).as_deref(),
        Some("finished")
    );
}

#[test]
fn codex_input_preserves_text_and_local_images() {
    let input = multimodal_input_items(&ChatInput {
        text: "What is shown here?".to_owned(),
        local_images: vec!["/tmp/example.png".into()],
    });
    assert_eq!(
        input[0],
        json!({"type": "text", "text": "What is shown here?"})
    );
    assert_eq!(
        input[1],
        json!({
            "type": "localImage",
            "path": "/tmp/example.png",
            "detail": "auto"
        })
    );
}

#[tokio::test]
async fn orientation_tool_requires_active_calibration() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-tools-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = Arc::new(
        ContinuityHost::open(store)
            .await
            .expect("open continuity host"),
    );
    let profile = Arc::new(
        ProfileStore::open(root.join("profile.toml"), root.join("orientation.md"))
            .await
            .expect("open profile"),
    );
    let tools = SymbiontTools::new(continuity, Arc::clone(&profile));
    let call = json!({
        "namespace": "symbiont",
        "tool": "complete_orientation",
        "arguments": {
            "orientation_markdown": "# Current Context\n\nBuilding symbiont-d."
        }
    });

    let rejected = tools.execute(&call).await;
    assert_eq!(rejected.response["success"], false);

    profile
        .begin(CalibrationMode::Guided)
        .await
        .expect("begin calibration");
    let accepted = tools.execute(&call).await;
    assert_eq!(accepted.response["success"], true);
    assert_eq!(profile.snapshot().await.status, SetupStatus::Ready);

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn pcp_tools_write_search_and_read_through_the_dynamic_bridge() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-pcp-tools-{nonce}"));
    let store = Arc::new(
        SqlitePcpStore::open(root.join("context.sqlite3"))
            .await
            .expect("open PCP store"),
    );
    let continuity = Arc::new(
        ContinuityHost::open(store)
            .await
            .expect("open continuity host"),
    );
    let profile = Arc::new(
        ProfileStore::open(root.join("profile.toml"), root.join("orientation.md"))
            .await
            .expect("open profile"),
    );
    let tools = SymbiontTools::new(continuity, profile);

    let written = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "write_page",
            "arguments": {
                "content": "The PCP bridge remembers a brass telescope.",
                "facets": {"kind": "bridge_test"},
                "source_refs": [{
                    "source_type": "test_url",
                    "uri": "https://example.com/telescope"
                }],
                "idempotency_key": "bridge-test"
            }
        }))
        .await;
    assert_eq!(written.response["success"], true);
    let written_json = tool_content_json(&written.response);
    let revision_id = written_json["revisionId"]
        .as_str()
        .expect("written revision id");
    let derived = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "write_page",
            "arguments": {
                "content": "The telescope is worth remembering.",
                "source_revision_ids": [revision_id],
                "idempotency_key": "bridge-derived-test"
            }
        }))
        .await;
    assert_eq!(derived.response["success"], true);
    let derived_json = tool_content_json(&derived.response);
    let derived_revision_id = derived_json["revisionId"]
        .as_str()
        .expect("derived revision id");

    let searched = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "search_pages",
            "arguments": {
                "query": "brass telescope",
                "mode": "exact"
            }
        }))
        .await;
    assert_eq!(searched.response["success"], true);
    let searched_json = tool_content_json(&searched.response);
    assert_eq!(searched_json["hits"][0]["revisionId"], revision_id);

    let read = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "read_pages",
            "arguments": {
                "revision_ids": [revision_id],
                "projections": ["payload", "facets"]
            }
        }))
        .await;
    assert_eq!(read.response["success"], true);
    let read_json = tool_content_json(&read.response);
    assert_eq!(
        read_json["pages"][0]["revision"]["payload"]["content"],
        "The PCP bridge remembers a brass telescope."
    );
    assert!(
        read_json["pages"][0]["revision"]
            .get("sourceRefs")
            .is_none()
    );
    assert!(
        read_json["pages"][0]["revision"]
            .get("provenance")
            .is_none()
    );

    let traced = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "read_pages",
            "arguments": {
                "revision_ids": [revision_id],
                "projections": ["sources", "provenance"]
            }
        }))
        .await;
    assert_eq!(traced.response["success"], true);
    let traced_json = tool_content_json(&traced.response);
    assert_eq!(
        traced_json["pages"][0]["revision"]["sourceRefs"][0]["uri"],
        "https://example.com/telescope"
    );
    assert_eq!(
        traced_json["pages"][0]["revision"]["provenance"][0]["operation"],
        "derive"
    );

    let derived_trace = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "read_pages",
            "arguments": {
                "revision_ids": [derived_revision_id],
                "projections": ["provenance", "relations"]
            }
        }))
        .await;
    assert_eq!(derived_trace.response["success"], true);
    let derived_trace_json = tool_content_json(&derived_trace.response);
    assert_eq!(
        derived_trace_json["pages"][0]["revision"]["provenance"][0]["inputRevisionIds"][0],
        revision_id
    );
    assert!(
        derived_trace_json["pages"][0]["relations"]
            .as_array()
            .expect("relations array")
            .is_empty()
    );

    let graph = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "search_pages",
            "arguments": {
                "query": revision_id,
                "mode": "graph",
                "filters": {"relation_types": ["derived_from"]}
            }
        }))
        .await;
    assert_eq!(graph.response["success"], true);
    let graph_json = tool_content_json(&graph.response);
    assert_eq!(graph_json["hits"][0]["revisionId"], derived_revision_id);

    let _ = tokio::fs::remove_dir_all(root).await;
}

fn tool_content_json(response: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(
        response["contentItems"][0]["text"]
            .as_str()
            .expect("tool text"),
    )
    .expect("tool JSON")
}
