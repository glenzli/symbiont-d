use std::{
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    client::{
        autonomous_response_is_superseded, context_revision_ids, extract_completed_response_text,
        extract_final_agent_message, generated_image_output, remember_generated_image,
        text_and_image_input_items,
    },
    prompts::{
        autonomous_exploration_prompt, developer_instructions, interaction_reflection_prompt,
        memory_reconciliation_prompt, summary_maintenance_prompt,
    },
    tools::SymbiontTools,
    trace::observable_item_event,
};
use crate::{
    compute_policy::ComputePolicyStore,
    continuity::{ContinuityHost, MessageLinks},
    curiosity::CuriosityStore,
    exploration::{ExplorationIntentQueue, ExplorationIntentReceiver},
    memory::MemoryRole,
    profile::{CalibrationMode, ProfileStore, SetupStatus},
    reconciliation::ReconciliationMode,
    reflection::ReflectionStore,
    symbiont_context::SymbiontContextStore,
    task_execution::TaskExecutionQueue,
    usage::{InvocationRecord, ToolTraceStep},
};
use pcp_sqlite::SqlitePcpStore;
use serde_json::json;

#[test]
fn dynamic_tools_expose_host_and_pcp_namespaces() {
    let specs = SymbiontTools::specifications();
    assert_eq!(specs[0]["type"], "namespace");
    assert_eq!(specs[0]["name"], "symbiont");
    assert_eq!(specs[0]["tools"][0]["name"], "complete_orientation");
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == "request_exploration" })
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "update_current_map")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "open_hunch")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "upsert_episode")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "upsert_interaction_hypothesis")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "reserve_continuation")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "propose_proactive_message")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "complete_reconciliation")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "escalate")
    );
    assert_eq!(specs[1]["name"], "pcp");
    assert_eq!(specs[1]["tools"][0]["name"], "describe");
    assert_eq!(specs[1]["tools"][2]["name"], "search_pages");
    assert_eq!(specs[1]["tools"][3]["name"], "read_pages");
    assert_eq!(specs[1]["tools"][4]["name"], "assess_validity");
    assert_eq!(specs[1]["tools"][5]["name"], "write_summary");
    assert_eq!(specs[1]["tools"][6]["name"], "write_page");
}

#[test]
fn summary_maintenance_keeps_the_model_on_one_exact_revision() {
    let prompt = summary_maintenance_prompt("rev_target", "<done/>");
    assert!(prompt.contains("exactly `rev_target`"));
    assert!(prompt.contains("Read that Revision's payload"));
    assert!(prompt.contains("do not write one"));
    assert!(prompt.contains("return exactly `<done/>`"));
}

#[test]
fn reconciliation_preview_is_concise_and_explicitly_read_only() {
    let prompt = memory_reconciliation_prompt(
        ReconciliationMode::Preview,
        "rec_test",
        r#"{"durablePages":[],"topicEpisodes":[]}"#,
        &[],
        "<done/>",
    );
    assert!(prompt.contains("read-only preview"));
    assert!(prompt.contains("reject every Page, Summary, Relation, and validity mutation"));
    assert!(prompt.contains("Prefer no-op over cosmetic organization"));
    assert!(prompt.contains("complete_reconciliation"));
}

#[test]
fn persistent_instructions_define_a_short_unambiguous_pcp_boundary() {
    let instructions = developer_instructions();
    assert!(instructions.contains("PCP is the user-owned long-term archive"));
    assert!(instructions.contains("before asking the user to repeat known history"));
    assert!(instructions.contains("absence means unreviewed, not invalid"));
    assert!(instructions.contains("Never treat validity as a hard filter"));
    assert!(instructions.contains("Do not repeat an identical PCP search or read"));
    assert!(instructions.contains("Rarely use `symbiont.reserve_continuation`"));
    assert!(instructions.contains("PCP memory operations remain available"));
    assert!(!instructions.contains("do not modify files or attempt side effects"));
    assert!(instructions.chars().count() < 3_500);
}

#[test]
fn context_revisions_ignore_failed_or_malformed_pcp_reads() {
    let valid = "rev_0123456789abcdef0123456789abcdef";
    let invocation = InvocationRecord {
        id: "turn_test".to_owned(),
        parent_id: None,
        thread_id: "thread_test".to_owned(),
        turn_id: "turn_test".to_owned(),
        origin: "autonomous".to_owned(),
        lane: "observe".to_owned(),
        requested_model: "test-model".to_owned(),
        effective_model: "test-model".to_owned(),
        model_display_name: "Test".to_owned(),
        effort: "medium".to_owned(),
        service_tier: None,
        started_at: "2026-07-31T00:00:00Z".to_owned(),
        completed_at: "2026-07-31T00:00:01Z".to_owned(),
        duration_ms: 1,
        status: "completed".to_owned(),
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        total_tokens: 0,
        tool_calls: Vec::new(),
        produced_message: false,
        trace_steps: vec![
            ToolTraceStep {
                sequence: 0,
                namespace: "pcp".to_owned(),
                tool: "read_pages".to_owned(),
                started_at: "2026-07-31T00:00:00Z".to_owned(),
                completed_at: "2026-07-31T00:00:00Z".to_owned(),
                duration_ms: 0,
                succeeded: false,
                arguments: json!({"revision_ids": [valid, "rev_89???"]}),
                result: json!({"success": false}),
            },
            ToolTraceStep {
                sequence: 1,
                namespace: "pcp".to_owned(),
                tool: "read_pages".to_owned(),
                started_at: "2026-07-31T00:00:01Z".to_owned(),
                completed_at: "2026-07-31T00:00:01Z".to_owned(),
                duration_ms: 0,
                succeeded: true,
                arguments: json!({"revision_ids": [valid, "rev_89???"]}),
                result: json!({"success": true}),
            },
        ],
        context_snapshot: None,
        trace_events: Vec::new(),
    };

    assert_eq!(context_revision_ids(&[invocation]), vec![valid.to_owned()]);
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
    assert_eq!(
        extract_completed_response_text(&params, "I am checking first. "),
        "finished"
    );
}

#[test]
fn autonomous_exploration_proposes_at_most_one_conversation() {
    assert!(autonomous_response_is_superseded(
        " <symbiont-superseded/>\n"
    ));

    let prompt = autonomous_exploration_prompt("<symbiont-silent/>", "<symbiont-superseded/>");
    assert!(prompt.contains("Search results are raw material"));
    assert!(prompt.contains("one conversational move"));
    assert!(prompt.contains("Never send a roundup"));
    assert!(prompt.contains("shortest natural bridge"));
    assert!(prompt.contains("older open thread"));
    assert!(prompt.contains("unsolicited"));
    assert!(prompt.contains("why it belongs in this conversation"));
    assert!(prompt.contains("answers only why now"));
    assert!(prompt.contains("pending attention"));
    assert!(prompt.contains("No process narration"));
    assert!(prompt.contains("already answered, invalidated"));
    assert!(prompt.contains("propose_proactive_message"));
    assert!(prompt.contains("Host rechecks timing"));
    assert!(prompt.contains("never put user-visible prose in the final response"));
}

#[test]
fn reflection_prompt_preserves_facts_uncertainty_and_profile_boundaries() {
    let prompt = interaction_reflection_prompt("<events/>", "<done/>");
    assert!(prompt.contains("Separate observed facts from inference"));
    assert!(prompt.contains("never ratings"));
    assert!(prompt.contains("Keep alternative explanations"));
    assert!(prompt.contains("do not force a tree"));
    assert!(prompt.contains("one-off questions"));
    assert!(prompt.contains("without scores or fixed message thresholds"));
    assert!(prompt.contains("message_revision_ids"));
    assert!(
        prompt.contains("same Revision may contribute to several Topics")
            || prompt.contains("same Revision may belong to several Topics")
    );
    assert!(prompt.contains("never promote temporary behavior directly"));
    assert!(prompt.contains("publication gate will still decide whether to speak"));
    assert!(prompt.contains("propose_proactive_message"));
    assert!(prompt.contains("why here and why now"));
    assert!(prompt.contains("feed item"));
    assert!(prompt.contains("pcp.assess_validity"));
    assert!(prompt.contains("not ordinary messages"));
    assert!(
        prompt.chars().count() < 3_200,
        "reflection prompt grew to {} characters",
        prompt.chars().count()
    );
}

#[test]
fn codex_input_preserves_text_and_local_images() {
    let input = text_and_image_input_items("What is shown here?", &["/tmp/example.png".into()]);
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

#[test]
fn codex_output_collects_generated_image_paths_once() {
    let item = json!({
        "type": "imageGeneration",
        "id": "image-1",
        "status": "completed",
        "result": "generated",
        "revisedPrompt": "A restrained abstract icon",
        "savedPath": "/tmp/generated-icon.png"
    });
    let image = generated_image_output(&item).expect("generated image output");
    assert_eq!(image.item_id, "image-1");
    assert_eq!(
        image.saved_path.to_string_lossy(),
        "/tmp/generated-icon.png"
    );
    assert_eq!(
        image.revised_prompt.as_deref(),
        Some("A restrained abstract icon")
    );

    let mut images = Vec::new();
    remember_generated_image(&mut images, image.clone());
    remember_generated_image(&mut images, image);
    assert_eq!(images.len(), 1);

    let (kind, title, details) = observable_item_event(&item).expect("trace image generation");
    assert!(matches!(kind, crate::diagnostics::TraceEventKind::ToolCall));
    assert_eq!(title, "Generated image");
    assert_eq!(details["savedPath"], "/tmp/generated-icon.png");
}

#[test]
fn trace_uses_reasoning_summaries_without_raw_reasoning_content() {
    let (_, _, details) = observable_item_event(&json!({
        "type": "reasoning",
        "summary": ["Checked the current context before searching memory."],
        "content": ["private raw reasoning"]
    }))
    .unwrap();

    assert_eq!(
        details["summary"][0],
        "Checked the current context before searching memory."
    );
    assert!(details.get("content").is_none());
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
    let context = Arc::new(SymbiontContextStore::new(Arc::clone(&continuity)));
    let curiosity = Arc::new(CuriosityStore::new(Arc::clone(&continuity)));
    let reflection = Arc::new(
        ReflectionStore::open(
            root.join("reflection.sqlite3"),
            root.join("reflection.toml"),
        )
        .await
        .expect("open Reflection store"),
    );
    let (exploration_intents, _exploration_intent_receiver) = test_exploration_intents(&root).await;
    let tools = SymbiontTools::new(
        continuity,
        Arc::clone(&profile),
        context,
        curiosity,
        reflection,
        Arc::new(
            ComputePolicyStore::open(root.join("compute-policies.toml"))
                .await
                .expect("open compute policies"),
        ),
        None,
        Arc::new(
            TaskExecutionQueue::open(root.join("task-runs.json"))
                .await
                .expect("open task execution queue")
                .0,
        ),
        Arc::new(crate::continuation::ContinuationQueue::new().0),
        exploration_intents,
    );
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
    let context = Arc::new(SymbiontContextStore::new(Arc::clone(&continuity)));
    let curiosity = Arc::new(CuriosityStore::new(Arc::clone(&continuity)));
    let reflection = Arc::new(
        ReflectionStore::open(
            root.join("reflection.sqlite3"),
            root.join("reflection.toml"),
        )
        .await
        .expect("open Reflection store"),
    );
    let (exploration_intents, _exploration_intent_receiver) = test_exploration_intents(&root).await;
    let tools = SymbiontTools::new(
        continuity,
        profile,
        context,
        curiosity,
        Arc::clone(&reflection),
        Arc::new(
            ComputePolicyStore::open(root.join("compute-policies.toml"))
                .await
                .expect("open compute policies"),
        ),
        None,
        Arc::new(
            TaskExecutionQueue::open(root.join("task-runs.json"))
                .await
                .expect("open task execution queue")
                .0,
        ),
        Arc::new(crate::continuation::ContinuationQueue::new().0),
        exploration_intents,
    );

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
    let assessed = tools
        .execute_for_model(
            &json!({
                "namespace": "pcp",
                "tool": "assess_validity",
                "arguments": {
                    "target_revision_id": revision_id,
                    "standing": "qualified",
                    "rationale": "The derived Page narrows why this detail remains useful.",
                    "scope": "Useful as a bridge fixture, not as a general observatory claim.",
                    "basis_revision_ids": [derived_revision_id],
                    "idempotency_key": "bridge-validity-test"
                }
            }),
            Some("test-model"),
            "maintenance",
        )
        .await;
    assert_eq!(assessed.response["success"], true);
    let summarized = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "write_summary",
            "arguments": {
                "target_revision_id": revision_id,
                "content": "A semantic observatory instrument used to test Summary routing.",
                "idempotency_key": "bridge-summary-test"
            }
        }))
        .await;
    assert_eq!(summarized.response["success"], true);

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
    assert_eq!(
        searched_json["hits"][0]["validity"]["standing"],
        "qualified"
    );

    let summary_search = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "search_pages",
            "arguments": {
                "query": "semantic observatory",
                "mode": "text",
                "projections": ["summary"]
            }
        }))
        .await;
    assert_eq!(summary_search.response["success"], true);
    let summary_search_json = tool_content_json(&summary_search.response);
    assert_eq!(summary_search_json["hits"][0]["revisionId"], revision_id);
    assert_eq!(
        summary_search_json["hits"][0]["matchedProjection"],
        "summary"
    );

    let read = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "read_pages",
            "arguments": {
                "revision_ids": [revision_id],
                "projections": ["summary", "validity", "payload", "facets"]
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
        read_json["pages"][0]["summary"]["content"]
            .as_str()
            .expect("summary content")
            .contains("semantic observatory")
    );
    assert_eq!(
        read_json["pages"][0]["validity"]["basisRevisionIds"][0],
        derived_revision_id
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
    assert!(
        graph_json["hits"]
            .as_array()
            .expect("graph hits")
            .iter()
            .any(|hit| hit["revisionId"] == derived_revision_id)
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn reflection_tools_accept_recalled_conversation_revisions_outside_the_event_tail() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-reflection-tools-{nonce}"));
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
    let recalled = continuity
        .ingest_message(
            MemoryRole::User,
            "An older conversation Revision can still support a new Episode.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("ingest recalled conversation source");
    let profile = Arc::new(
        ProfileStore::open(root.join("profile.toml"), root.join("orientation.md"))
            .await
            .expect("open profile"),
    );
    let context = Arc::new(SymbiontContextStore::new(Arc::clone(&continuity)));
    let curiosity = Arc::new(CuriosityStore::new(Arc::clone(&continuity)));
    let reflection = Arc::new(
        ReflectionStore::open(
            root.join("reflection.sqlite3"),
            root.join("reflection.toml"),
        )
        .await
        .expect("open Reflection store"),
    );
    let (exploration_intents, _exploration_intent_receiver) = test_exploration_intents(&root).await;
    let tools = SymbiontTools::new(
        continuity,
        profile,
        context,
        curiosity,
        Arc::clone(&reflection),
        Arc::new(
            ComputePolicyStore::open(root.join("compute-policies.toml"))
                .await
                .expect("open compute policies"),
        ),
        None,
        Arc::new(
            TaskExecutionQueue::open(root.join("task-runs.json"))
                .await
                .expect("open task execution queue")
                .0,
        ),
        Arc::new(crate::continuation::ContinuationQueue::new().0),
        Arc::clone(&exploration_intents),
    );

    let result = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "upsert_episode",
                "arguments": {
                    "title": "Recalled design line",
                    "summary": "The source is outside Reflection's imported event tail but remains auditable through PCP.",
                    "state": "active",
                    "source_revision_ids": [recalled.page.revision_id.clone()]
                }
            }),
            Some("test-model"),
            "reflection",
        )
        .await;
    assert_eq!(result.response["success"], true);

    let follow_up = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "schedule_follow_up",
                "arguments": {
                    "reason": "Reconsider this as a distinct continuation after the current exchange has settled.",
                    "not_before": (chrono::Utc::now() + chrono::Duration::minutes(2)).to_rfc3339(),
                    "source_revision_ids": [recalled.page.revision_id.clone()]
                }
            }),
            Some("test-model"),
            "interactive",
        )
        .await;
    assert_eq!(follow_up.response["success"], true);
    assert_eq!(reflection.follow_ups(10).await.unwrap().len(), 1);

    let exploration = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "request_exploration",
                "arguments": {
                    "question": "Does this older design constraint still hold in the current runtime?",
                    "why_now": "The recalled Revision changes what evidence the next implementation decision needs.",
                    "source_revision_ids": [recalled.page.revision_id]
                }
            }),
            Some("test-model"),
            "interactive",
        )
        .await;
    assert_eq!(exploration.response["success"], true);
    let exploration_json = tool_content_json(&exploration.response);
    assert_eq!(exploration_json["accepted"], true);
    assert_eq!(exploration_intents.recent(10).await.len(), 1);

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn hunch_tools_preserve_model_owned_state_and_record_autonomous_exploration() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-hunch-tools-{nonce}"));
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
    let source = continuity
        .ingest_message(
            crate::memory::MemoryRole::User,
            "Could this conversation become an exploration trigger?",
            Vec::new(),
            None,
            crate::continuity::MessageLinks::default(),
        )
        .await
        .expect("store source");
    let profile = Arc::new(
        ProfileStore::open(root.join("profile.toml"), root.join("orientation.md"))
            .await
            .expect("open profile"),
    );
    let context = Arc::new(SymbiontContextStore::new(Arc::clone(&continuity)));
    let curiosity = Arc::new(CuriosityStore::new(Arc::clone(&continuity)));
    let reflection = Arc::new(
        ReflectionStore::open(
            root.join("reflection.sqlite3"),
            root.join("reflection.toml"),
        )
        .await
        .expect("open Reflection store"),
    );
    let (exploration_intents, _exploration_intent_receiver) = test_exploration_intents(&root).await;
    let tools = SymbiontTools::new(
        continuity,
        profile,
        context,
        Arc::clone(&curiosity),
        reflection,
        Arc::new(
            ComputePolicyStore::open(root.join("compute-policies.toml"))
                .await
                .expect("open compute policies"),
        ),
        None,
        Arc::new(
            TaskExecutionQueue::open(root.join("task-runs.json"))
                .await
                .expect("open task execution queue")
                .0,
        ),
        Arc::new(crate::continuation::ContinuationQueue::new().0),
        exploration_intents,
    );

    let opened = tools
        .execute(&json!({
            "namespace": "symbiont",
            "tool": "open_hunch",
            "arguments": {
                "question": "Does conversation-triggered exploration diversify the search?",
                "origin": "symbiont",
                "why_alive": "Scheduled runs have repeated nearby themes.",
                "what_would_change_it": "Several event-driven runs produce distinct evidence.",
                "source_revision_ids": [source.page.revision_id.clone()]
            }
        }))
        .await;
    assert_eq!(opened.response["success"], true);
    let opened_json = tool_content_json(&opened.response);
    let page_id = opened_json["pageId"].as_str().expect("Hunch Page");
    let revision_id = opened_json["revisionId"].as_str().expect("Hunch Revision");

    let revised = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "revise_hunch",
                "arguments": {
                    "page_id": page_id,
                    "expected_revision_id": revision_id,
                    "state": "watching"
                }
            }),
            Some("test-model"),
            "autonomous",
        )
        .await;
    assert_eq!(revised.response["success"], true);
    let snapshot = curiosity.snapshot().await.expect("read curiosity");
    assert_eq!(snapshot.active_count, 1);
    assert_eq!(snapshot.hunches[0].origin.as_str(), "symbiont");
    assert!(snapshot.hunches[0].last_explored_at.is_some());

    let candidate = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "propose_proactive_message",
                "arguments": {
                    "message": "这里有个值得接着讨论的变化。",
                    "reason": "The autonomous run found evidence that changes the open question.",
                    "source_revision_ids": [source.page.revision_id]
                }
            }),
            Some("test-model"),
            "autonomous",
        )
        .await;
    assert_eq!(candidate.response["success"], true);

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

async fn test_exploration_intents(
    root: &Path,
) -> (Arc<ExplorationIntentQueue>, ExplorationIntentReceiver) {
    let (queue, receiver) = ExplorationIntentQueue::open(root.join("exploration-intents.json"))
        .await
        .expect("open exploration intent queue");
    (Arc::new(queue), receiver)
}
