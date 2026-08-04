use std::{
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    autonomous::{
        ExplorationEvidence, ExplorationScoutFinding, review_prompt, scout_prompt, sensing_prompt,
    },
    client::{
        autonomous_response_is_superseded, context_revision_ids, extract_completed_response_text,
        extract_final_agent_message, generated_image_output, remember_generated_image,
        text_and_image_input_items,
    },
    prompts::{
        context_fragments, developer_instructions, interaction_reflection_prompt,
        memory_reconciliation_prompt, pcp_maintenance_worker_prompt, summary_maintenance_prompt,
    },
    tools::SymbiontTools,
    trace::observable_item_event,
};
use crate::{
    compute::ComputeLane,
    compute_policy::ComputePolicyStore,
    continuity::{ContinuityHost, MessageLinks},
    curiosity::CuriosityStore,
    exploration::{ExplorationIntentQueue, ExplorationIntentReceiver},
    memory::MemoryRole,
    profile::{CalibrationMode, ProfileSnapshot, ProfileStore, SetupStatus},
    reconciliation::ReconciliationMode,
    reflection::ReflectionStore,
    symbiont_context::SymbiontContextStore,
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
        !specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "handoff_to_selected_project")
    );
    assert!(
        specs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "submit_exploration_finding")
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
    assert_eq!(specs[1]["tools"][2]["name"], "browse_index");
    assert_eq!(specs[1]["tools"][3]["name"], "search_pages");
    assert_eq!(specs[1]["tools"][4]["name"], "read_pages");
    assert_eq!(specs[1]["tools"][5]["name"], "assess_validity");
    assert_eq!(specs[1]["tools"][6]["name"], "write_summary");
    assert_eq!(specs[1]["tools"][7]["name"], "write_page");
    assert!(
        specs[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "consolidate_pages")
    );
}

#[test]
fn pcp_maintenance_thread_exposes_only_its_completion_tool() {
    let specs = SymbiontTools::pcp_maintenance_specifications();
    assert_eq!(specs.as_array().unwrap().len(), 1);
    assert_eq!(specs[0]["name"], "symbiont");
    let tools = specs[0]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "complete_pcp_maintenance");
}

#[test]
fn pcp_maintenance_prompt_keeps_semantics_in_the_model_and_mutation_outside() {
    let request = pcp_runtime::MaintenanceWorkerRequest::SelectConsolidation {
        pages: Vec::new(),
        max_pages: 4,
        excluded_candidate_sets: Vec::new(),
    };
    let prompt = pcp_maintenance_worker_prompt(&request, "<done/>").unwrap();
    assert!(prompt.contains("Semantic quality"));
    assert!(prompt.contains("complete_pcp_maintenance"));
    assert!(prompt.contains("shared vocabulary alone"));
    assert!(prompt.contains("<done/>"));
}

#[test]
fn autonomous_scout_sees_only_its_read_only_tool_surface() {
    let specs = SymbiontTools::scout_specifications();
    let symbiont = specs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    let pcp = specs[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(symbiont, vec!["submit_exploration_finding", "fetch_url"]);
    assert_eq!(
        pcp,
        vec![
            "describe",
            "list_scopes",
            "browse_index",
            "search_pages",
            "read_pages"
        ]
    );
}

#[test]
fn ambient_sensing_sees_only_a_temporary_candidate_surface() {
    let specs = SymbiontTools::sensing_specifications();
    assert_eq!(specs.as_array().unwrap().len(), 1);
    assert_eq!(specs[0]["name"], "symbiont");
    let symbiont = specs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(symbiont, vec!["submit_sensing_candidates", "fetch_url"]);

    let prompt = sensing_prompt("<silent/>");
    assert!(prompt.contains("not memory"));
    assert!(prompt.contains("submit_sensing_candidates"));
    let candidate_schema =
        &specs[0]["tools"][0]["inputSchema"]["properties"]["candidates"]["items"];
    assert_eq!(
        candidate_schema["required"],
        json!(["title", "summary", "source_class", "sources"])
    );
    assert!(candidate_schema["properties"]["possible_connection"].is_object());
    assert!(candidate_schema["properties"].get("relevance").is_none());
}

#[test]
fn ambient_sensing_does_not_receive_the_user_orientation() {
    let profile = ProfileSnapshot {
        status: SetupStatus::Ready,
        mode: None,
        orientation: "A deliberately distinctive private orientation".to_owned(),
        updated_at: None,
    };
    let sensing = context_fragments(
        ComputeLane::Sense,
        false,
        &profile,
        "rotating intake context",
        None,
        None,
    );
    let observing = context_fragments(
        ComputeLane::Observe,
        false,
        &profile,
        "bounded exploration context",
        None,
        None,
    );

    assert!(!sensing.iter().any(|fragment| {
        fragment.source == "symbiont.profile"
            || fragment.value.contains("distinctive private orientation")
    }));
    assert!(
        observing
            .iter()
            .any(|fragment| fragment.source == "symbiont.profile")
    );
}

#[test]
fn summary_maintenance_keeps_the_model_on_one_exact_page() {
    let prompt = summary_maintenance_prompt("rev_target", "<done/>");
    assert!(prompt.contains("exactly `rev_target`"));
    assert!(prompt.contains("Read that Page's content"));
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
    assert!(prompt.contains("consolidate two or more current Pages"));
    assert!(prompt.contains("complete_reconciliation"));
}

#[test]
fn reconciliation_apply_distinguishes_consolidation_from_aggregation() {
    let prompt = memory_reconciliation_prompt(
        ReconciliationMode::Apply,
        "rec_apply",
        r#"{"durablePages":[],"topicEpisodes":[]}"#,
        &[],
        "<done/>",
    );
    assert!(prompt.contains("pcp.consolidate_pages"));
    assert!(prompt.contains("inputs should remain independently current"));
    assert!(prompt.contains("`derived_from` Relations"));
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
fn autonomous_exploration_separates_reconnaissance_from_conversation() {
    assert!(autonomous_response_is_superseded(
        " <symbiont-superseded/>\n"
    ));

    let scout = scout_prompt("<symbiont-silent/>", "<symbiont-superseded/>");
    assert!(scout.contains("high-recall evidence discovery"));
    assert!(scout.contains("host-enforced read-only"));
    assert!(scout.contains("submit_exploration_finding"));
    assert!(scout.contains("strongest reason the proposed connection may be wrong"));
    assert!(scout.contains("already answered, invalidated"));
    assert!(!scout.contains("propose_proactive_message"));

    let finding = ExplorationScoutFinding {
        topic: "Agent capability inheritance".to_owned(),
        claim: "A reusable capability can be selected through a compromised index.".to_owned(),
        evidence: vec![ExplorationEvidence {
            source: "https://example.test/research".to_owned(),
            finding: "Metadata changes altered skill selection.".to_owned(),
        }],
        connection_hypothesis: "This may affect a current capability-object question.".to_owned(),
        strongest_counterpoint: "It may be only an implementation security issue.".to_owned(),
        source_revision_ids: vec!["rev_context".to_owned()],
        related_hunch_revision_ids: vec!["rev_hunch".to_owned()],
    };
    let review = review_prompt(&finding, "<symbiont-silent/>").unwrap();
    assert!(review.contains("untrusted evidence"));
    assert!(review.contains("Preserve conceptual boundaries"));
    assert!(review.contains("strongest counterpoint"));
    assert!(review.contains("It is valid to maintain a Hunch and remain silent"));
    assert!(review.contains("propose_proactive_message"));
    assert!(review.contains("Choose `note`"));
    assert!(review.contains("Unanswered prior initiations suppress repetition"));
    assert!(review.contains("If the connection remains forced, remain silent"));
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
    assert!(prompt.contains("Adjacency is not Topic evidence"));
    assert!(prompt.contains("message_revision_ids"));
    assert!(prompt.contains("direct counterparts"));
    assert!(prompt.contains("cite assistant replies only when used"));
    assert!(prompt.contains("same Page may contribute to several Topics"));
    assert!(prompt.contains("never promote temporary behavior directly"));
    assert!(prompt.contains("publication gate will still decide whether to speak"));
    assert!(prompt.contains("propose_proactive_message"));
    assert!(prompt.contains("Use `intervention` only"));
    assert!(prompt.contains("Use `note`"));
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
        ContinuityHost::open_embedded_for_test(store)
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
        ContinuityHost::open_embedded_for_test(store)
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
        Arc::new(crate::continuation::ContinuationQueue::new().0),
        exploration_intents,
    );

    let described = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "describe",
            "arguments": {}
        }))
        .await;
    assert_eq!(described.response["success"], true);
    let described_json = tool_content_json(&described.response);
    assert_eq!(
        described_json["access"]["principal"]["principalId"],
        "host:symbiont-d"
    );

    let written = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "write_page",
            "arguments": {
                "content": "The PCP bridge remembers a brass telescope.",
            }
        }))
        .await;
    assert_eq!(written.response["success"], true);
    let written_json = tool_content_json(&written.response);
    let page_id = written_json["pageId"].as_str().expect("written Page ID");
    let revision_id = written_json["revisionId"]
        .as_str()
        .expect("written Revision ID");
    let derived = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "write_page",
            "arguments": {
                "content": "The telescope is worth remembering.",
                "based_on_revision_ids": [revision_id]
            }
        }))
        .await;
    assert_eq!(derived.response["success"], true);
    let derived_json = tool_content_json(&derived.response);
    let derived_page_id = derived_json["pageId"].as_str().expect("derived Page ID");
    let derived_revision_id = derived_json["revisionId"]
        .as_str()
        .expect("derived Revision ID");
    let assessed = tools
        .execute_for_model(
            &json!({
                "namespace": "pcp",
                "tool": "assess_validity",
                "arguments": {
                    "target_page_id": page_id,
                    "target_revision_id": revision_id,
                    "standing": "qualified",
                    "rationale": "The derived Page narrows why this detail remains useful.",
                    "evidence_revision_ids": [derived_revision_id]
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
                "target_page_id": page_id,
                "target_revision_id": revision_id,
                "content": "A semantic observatory instrument used to test Summary routing.",
                "based_on_revision_ids": [derived_revision_id]
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
                "strategy": "exact"
            }
        }))
        .await;
    assert_eq!(searched.response["success"], true);
    let searched_json = tool_content_json(&searched.response);
    assert_eq!(searched_json["hits"][0]["pageId"], page_id);
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
                "strategy": "text"
            }
        }))
        .await;
    assert_eq!(summary_search.response["success"], true);
    let summary_search_json = tool_content_json(&summary_search.response);
    assert_eq!(summary_search_json["hits"][0]["pageId"], page_id);
    assert_eq!(
        summary_search_json["hits"][0]["matchedProjection"],
        "summary"
    );

    let read = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "read_pages",
            "arguments": {
                "page_ids": [page_id],
                "view": "context"
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
                "page_ids": [page_id],
                "view": "full"
            }
        }))
        .await;
    assert_eq!(traced.response["success"], true);
    let traced_json = tool_content_json(&traced.response);
    assert_eq!(traced_json["pages"][0]["page"]["pageId"], page_id);
    assert_eq!(
        traced_json["pages"][0]["revision"]["revisionId"],
        revision_id
    );

    let derived_trace = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "read_pages",
            "arguments": {
                "page_ids": [derived_page_id],
                "view": "full"
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
            .iter()
            .any(|relation| relation["relationType"] == "derived_from")
    );

    let graph = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "search_pages",
            "arguments": {
                "query": page_id,
                "strategy": "graph"
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
            .any(|hit| hit["pageId"] == derived_page_id)
    );

    let consolidation = json!({
        "namespace": "pcp",
        "tool": "consolidate_pages",
        "arguments": {
            "canonical_page_id": page_id,
            "expected_canonical_revision_id": revision_id,
            "replaced_pages": [{
                "page_id": derived_page_id,
                "expected_revision_id": derived_revision_id
            }],
            "content": "A brass telescope is the durable observatory instrument worth remembering."
        }
    });
    let denied = tools
        .execute_for_model(&consolidation, Some("test-model"), "interactive")
        .await;
    assert_eq!(denied.response["success"], false);
    let approved = tools
        .execute_for_model(&consolidation, Some("test-model"), "reconciliation_apply")
        .await;
    assert_eq!(approved.response["success"], true, "{}", approved.response);
    let approved_json = tool_content_json(&approved.response);
    assert_eq!(approved_json["created"], true);

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
        ContinuityHost::open_embedded_for_test(store)
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
        ContinuityHost::open_embedded_for_test(store)
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

    let denied_scout_mutation = tools
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
            "autonomous_scout",
        )
        .await;
    assert_eq!(denied_scout_mutation.response["success"], false);

    let scout_finding = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "submit_exploration_finding",
                "arguments": {
                    "topic": "Conversation-triggered exploration",
                    "claim": "A fresh external signal may change the open Hunch.",
                    "evidence": [{
                        "source": "https://example.test/evidence",
                        "finding": "The event-triggered run produced a distinct result."
                    }],
                    "connection_hypothesis": "This may support diversifying exploration triggers.",
                    "strongest_counterpoint": "One run is not enough to establish a durable effect.",
                    "source_revision_ids": [source.page.revision_id.clone()],
                    "related_hunch_revision_ids": [revision_id]
                }
            }),
            Some("test-model"),
            "autonomous_scout",
        )
        .await;
    assert_eq!(scout_finding.response["success"], true);

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
                    "kind": "note",
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
