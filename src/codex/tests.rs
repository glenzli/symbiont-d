use std::{
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    autonomous::{ExplorationEvidence, ExplorationScoutFinding, review_prompt, scout_prompt},
    client::{
        autonomous_response_is_superseded, context_revision_ids, extract_completed_response_text,
        extract_final_agent_message, generated_image_output, remember_generated_image,
        should_restart_app_server, text_and_image_input_items,
    },
    prompts::{
        context_fragments, developer_instructions, interaction_reflection_prompt,
        temporary_discussion_developer_instructions,
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
    reflection::ReflectionStore,
    symbiont_context::SymbiontContextStore,
    usage::{InvocationRecord, ToolTraceStep},
};
use pcp_sqlite::SqlitePcpStore;
use serde_json::json;

#[test]
fn terminal_reconnecting_errors_are_connection_failures() {
    let error = anyhow::anyhow!("Reconnecting... 5/5");
    assert!(should_restart_app_server(&error));
}

#[test]
fn pcp_feedback_exposes_optional_correction_evidence() {
    let specs = SymbiontTools::specifications();
    let pcp = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|spec| spec["name"] == "pcp")
        .unwrap();
    let feedback = pcp["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "submit_feedback")
        .unwrap();
    let schema = &feedback["inputSchema"];
    assert_eq!(
        schema["properties"]["evidence_revision_ids"]["type"],
        "array"
    );
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("evidence_revision_ids"))
    );
}

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
        !specs[0]["tools"]
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
    assert_eq!(specs[1]["tools"][4]["name"], "semantic_search");
    assert_eq!(specs[1]["tools"][5]["name"], "match_intent");
    assert_eq!(specs[1]["tools"][6]["name"], "read_pages");
    assert!(
        specs[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "write_page")
    );
    assert!(!specs[1]["tools"].as_array().unwrap().iter().any(|tool| {
        matches!(
            tool["name"].as_str(),
            Some(
                "assess_validity"
                    | "write_summary"
                    | "revise_page"
                    | "relate_pages"
                    | "consolidate_pages"
            )
        )
    }));
}

#[test]
fn all_dynamic_tool_surfaces_use_consistent_canonical_types() {
    // App-server rejects an entire thread/start when even one function omits
    // its type and mixes the legacy shape into a canonical namespace.
    for specs in [
        SymbiontTools::specifications(),
        SymbiontTools::conversation_specifications(),
        SymbiontTools::sensing_specifications(),
        SymbiontTools::scout_specifications(),
        SymbiontTools::attacker_specifications(),
    ] {
        for namespace in specs.as_array().unwrap() {
            assert_eq!(namespace["type"], "namespace", "{namespace}");
            for tool in namespace["tools"].as_array().unwrap() {
                assert_eq!(tool["type"], "function", "{tool}");
                assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            }
        }
    }
}

#[test]
fn foreground_tools_keep_autonomous_memory_but_not_background_bookkeeping() {
    let specs = SymbiontTools::conversation_specifications();
    let names = specs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"search_transcript"));
    assert!(names.contains(&"read_background_context"));
    for background in [
        "upsert_episode",
        "upsert_interaction_hypothesis",
        "complete_reflection",
        "update_current_map",
        "submit_sensing_candidates",
    ] {
        assert!(!names.contains(&background));
    }
    assert!(
        specs[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "write_page")
    );
    assert!(
        specs[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "semantic_search")
    );
}

#[test]
fn context_provenance_matches_sent_fragments_and_deduplicates_the_bridge() {
    let mut bundle = crate::context_assembly::ContextBundle::single(
        "symbiont.recall_status",
        "host",
        "status",
        "available".into(),
    );
    bundle.include(
        "symbiont.transcript.msg_1",
        "local",
        "match",
        "duplicate text".into(),
    );
    bundle.defer_background();
    let profile = ProfileSnapshot {
        status: SetupStatus::Ready,
        mode: None,
        orientation: "known preference".into(),
        updated_at: None,
    };
    let bridge = crate::working_context::WorkingContext {
        cursor_before: None,
        current_revision_id: Some("msg_1".into()),
        reply_to_revision_id: None,
        reason: crate::working_context::WorkingContextReason::ThreadStart,
        truncated: false,
        messages: Vec::new(),
    };
    let fragments = context_fragments(
        ComputeLane::Conversation,
        false,
        &profile,
        &bundle,
        Some(&bridge),
        None,
    );
    assert!(
        !fragments
            .iter()
            .any(|part| part.source == "symbiont.transcript.msg_1")
    );
    assert!(!fragments.iter().any(
        |part| part.source == "symbiont.pcp" || part.source.starts_with("symbiont.background.")
    ));
    let audit = crate::context_assembly::audit_fragments(&fragments, &bundle.selection);
    let sent = super::prompts::additional_context_value(&fragments);
    for row in audit {
        assert_eq!(row.included, sent.get(&row.source).is_some());
    }
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

    assert_eq!(symbiont, vec!["submit_exploration_finding"]);
    assert_eq!(
        pcp,
        vec![
            "describe",
            "list_scopes",
            "browse_index",
            "search_pages",
            "semantic_search",
            "match_intent",
            "read_pages"
        ]
    );
}

#[test]
fn attacker_sees_only_its_single_publication_gate() {
    let specs = SymbiontTools::attacker_specifications();
    assert_eq!(specs.as_array().unwrap().len(), 1);
    let tools = specs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(tools, vec!["submit_attacker_assessment"]);
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
        &crate::context_assembly::ContextBundle::single(
            "symbiont.intake",
            "intake",
            "sensing",
            "rotating intake context".into(),
        ),
        None,
        None,
    );
    let observing = context_fragments(
        ComputeLane::Observe,
        false,
        &profile,
        &crate::context_assembly::ContextBundle::single(
            "symbiont.exploration",
            "exploration",
            "scout",
            "bounded exploration context".into(),
        ),
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
fn persistent_instructions_define_a_short_unambiguous_pcp_boundary() {
    let instructions = developer_instructions();
    assert!(instructions.contains("PCP is a compound context system"));
    assert!(instructions.contains("Host-local source plane owns raw user and assistant"));
    assert!(instructions.contains("PCP Runtime owns retained cross-Host Pages"));
    assert!(instructions.contains("plausible future value"));
    assert!(instructions.contains("It need not be verified, exceptional, or polished"));
    assert!(instructions.contains("before asking the user to repeat known history"));
    assert!(instructions.contains("Autonomously call `pcp.write_page`"));
    assert!(instructions.contains("Do not mirror every turn"));
    assert!(instructions.contains("Do not repeat an identical PCP search or read"));
    assert!(instructions.contains("Rarely use `symbiont.reserve_continuation`"));
    assert!(instructions.contains("PCP memory operations remain available"));
    assert!(!instructions.contains("do not modify files or attempt side effects"));
    assert!(instructions.chars().count() < 4_500);
}

#[test]
fn temporary_discussion_changes_retention_without_changing_identity() {
    let instructions = temporary_discussion_developer_instructions();
    assert!(instructions.contains("same conversational quality"));
    assert!(instructions.contains("will not write this exchange to PCP"));
    assert!(instructions.contains("No Symbiont dynamic tools are available"));
    assert!(instructions.contains("Web search may be used"));
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
    assert!(scout.contains("strongest reason the proposed connection"));
    assert!(scout.contains("already answered, invalidated"));
    assert!(scout.contains("user may already know the headline"));
    assert!(scout.contains("empty list"));
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
    assert!(review.contains("Choose `discussion`"));
    assert!(review.contains("user may already know it"));
    assert!(review.contains("Unanswered prior initiations suppress repetition"));
    assert!(review.contains("If a claimed connection remains forced"));

    let tools = SymbiontTools::specifications();
    let proactive = tools[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "propose_proactive_message")
        .unwrap();
    assert_eq!(
        proactive["inputSchema"]["properties"]["kind"]["enum"],
        json!(["intervention", "note", "discussion"])
    );
    assert_eq!(
        proactive["inputSchema"]["properties"]["source_revision_ids"]["minItems"],
        0
    );
}

#[test]
fn reflection_prompt_preserves_facts_uncertainty_and_profile_boundaries() {
    let prompt = interaction_reflection_prompt("<events/>", "<done/>");
    assert!(prompt.contains("Separate observed facts from inference"));
    assert!(prompt.contains("never ratings"));
    assert!(prompt.contains("Keep alternative explanations"));
    assert!(prompt.contains("do not force a tree"));
    assert!(prompt.contains("one-off questions"));
    assert!(prompt.contains("three user-authored turns"));
    assert!(prompt.contains("Adjacent two-turn discussion is not enough"));
    assert!(prompt.contains("message_revision_ids"));
    assert!(prompt.contains("direct counterparts"));
    assert!(prompt.contains("cite assistant replies only when used"));
    assert!(prompt.contains("same Page may contribute to several Topics"));
    assert!(prompt.contains("never promote temporary behavior directly"));
    assert!(prompt.contains("publication gate will still decide whether to speak"));
    assert!(prompt.contains("propose_proactive_message"));
    assert!(prompt.contains("`intervention` changes"));
    assert!(prompt.contains("`note` adds"));
    assert!(prompt.contains("`discussion` opens"));
    assert!(prompt.contains("feed"));
    assert!(prompt.contains("new self-contained PCP Page"));
    assert!(prompt.contains("tenant-side validity"));
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
        Arc::clone(&continuity),
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
async fn pcp_tools_defer_without_query_and_preserve_read_feedback_contracts() {
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
    let source_message = continuity
        .ingest_message(
            MemoryRole::User,
            "Please remember that the observatory uses a brass telescope.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("store local transcript source");
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
        Arc::clone(&continuity),
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

    for section in ["map", "curiosity", "reflection", "compute_policies"] {
        let read = tools.execute_for_model(&json!({
            "namespace": "symbiont", "tool": "read_background_context", "arguments": {"section": section}
        }), Some("test-model"), "interactive").await;
        assert!(read.succeeded, "{}", read.response);
        assert_eq!(
            tool_content_json(&read.response)["source"],
            "host-local-background"
        );
    }
    let rejected = tools.execute_for_model(&json!({
        "namespace": "symbiont", "tool": "read_background_context", "arguments": {"section": "reflection"}
    }), Some("test-model"), "luna_sense").await;
    assert!(!rejected.succeeded);

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
                "kind": "project_fact",
                "content": "The PCP bridge remembers a brass telescope.",
                "source_message_ids": [source_message.page.revision_id],
            }
        }))
        .await;
    assert_eq!(
        written.response["success"], true,
        "PCP write failed: {}",
        written.response
    );
    let written_json = tool_content_json(&written.response);
    assert_eq!(written_json["status"], "deferred");
    assert_eq!(written_json["created"], false);
    // This fixture has no Runtime query provider. Seed existing library data
    // directly for the separate read/feedback assertions below; production
    // tool writes must remain deferred instead of bypassing preflight.
    let fixture_page = continuity
        .write_model_page(
            None,
            "The PCP bridge remembers a brass telescope.",
            Some(json!({"kind":"project_fact"})),
            continuity
                .transcript_source_refs(&[source_message.page.revision_id.clone()])
                .await
                .unwrap(),
            vec![],
            vec![],
            None,
        )
        .await
        .unwrap();
    let written_json = json!({"pageId":fixture_page.page_id,"revisionId":fixture_page.revision_id});
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
    assert_eq!(
        derived.response["success"], true,
        "derived PCP write failed: {}",
        derived.response
    );
    let derived_json = tool_content_json(&derived.response);
    assert_eq!(derived_json["status"], "deferred");
    let fixture_derived = continuity
        .write_model_page(
            None,
            "The telescope is worth remembering.",
            None,
            vec![],
            vec![revision_id.to_owned()],
            vec![],
            None,
        )
        .await
        .unwrap();
    let derived_json =
        json!({"pageId":fixture_derived.page_id,"revisionId":fixture_derived.revision_id});
    let derived_page_id = derived_json["pageId"].as_str().expect("derived Page ID");
    let derived_revision_id = derived_json["revisionId"]
        .as_str()
        .expect("derived Revision ID");
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
    assert_eq!(
        traced_json["pages"][0]["revision"]["sourceRefs"][0]["providerId"],
        "symbiont:transcript"
    );
    let transcript_locator = traced_json["pages"][0]["revision"]["sourceRefs"][0]["locator"]
        .as_str()
        .expect("transcript locator");
    assert!(transcript_locator.starts_with("store/src_"));
    assert!(transcript_locator.contains("/message/msg_"));
    assert!(
        traced_json["pages"][0]["revision"]["sourceRefs"][0]["contentDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
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
    assert_eq!(
        derived_trace_json["pages"][0]["revision"]["provenance"][0]["operation"],
        "ingest"
    );
    assert!(
        derived_trace_json["pages"][0]["relations"]
            .as_array()
            .expect("relations array")
            .is_empty()
    );

    let correction_message = continuity
        .ingest_message(
            MemoryRole::User,
            "Correction: the telescope is bronze, not brass.",
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .expect("store local correction source");
    let feedback = tools
        .execute_for_model(
            &json!({
                "namespace": "pcp",
                "tool": "submit_feedback",
                "arguments": {
                    "kind": "correction",
                    "authority": "subject_owner",
                    "content": "The user clarifies that the telescope is bronze, not brass.",
                    "source_message_ids": [correction_message.page.revision_id],
                    "challenged_revision_ids": [revision_id],
                    "used_revision_ids": [revision_id, derived_revision_id]
                }
            }),
            Some("test-model"),
            "interactive",
        )
        .await;
    assert_eq!(feedback.response["success"], true, "{}", feedback.response);
    let feedback_json = tool_content_json(&feedback.response);
    assert_eq!(feedback_json["challengedRevisionIds"][0], revision_id);
    assert_eq!(feedback_json["evidenceRevisionIds"], json!([]));
    assert!(
        feedback_json["feedbackRevisionId"]
            .as_str()
            .is_some_and(|revision| revision.starts_with("rev_"))
    );

    // New correction evidence must remain distinct from what the old answer used.
    let evidence = tools
        .execute(&json!({
            "namespace": "pcp",
            "tool": "write_page",
            "arguments": {
                "kind": "project_fact",
                "content": "The user confirms that the observatory telescope is bronze.",
                "source_message_ids": [correction_message.page.revision_id]
            }
        }))
        .await;
    assert_eq!(evidence.response["success"], true, "{}", evidence.response);
    let evidence_json = tool_content_json(&evidence.response);
    assert_eq!(evidence_json["status"], "deferred");
    let fixture_evidence = continuity
        .write_model_page(
            None,
            "The user confirms that the observatory telescope is bronze.",
            Some(json!({"kind":"project_fact"})),
            continuity
                .transcript_source_refs(&[correction_message.page.revision_id.clone()])
                .await
                .unwrap(),
            vec![],
            vec![],
            None,
        )
        .await
        .unwrap();
    let evidence_json = json!({"revisionId":fixture_evidence.revision_id});
    let evidence_revision_id = evidence_json["revisionId"].as_str().unwrap();
    let correction_with_evidence = json!({
        "namespace": "pcp",
        "tool": "submit_feedback",
        "arguments": {
            "kind": "correction",
            "authority": "subject_owner",
            "content": "The user clarifies that the telescope is bronze, not brass.",
            "source_message_ids": [correction_message.page.revision_id],
            "challenged_revision_ids": [revision_id],
            "used_revision_ids": [revision_id, derived_revision_id],
            "evidence_revision_ids": [evidence_revision_id]
        }
    });
    let supported_feedback = tools
        .execute_for_model(&correction_with_evidence, Some("test-model"), "interactive")
        .await;
    assert_eq!(
        supported_feedback.response["success"], true,
        "{}",
        supported_feedback.response
    );
    let supported_json = tool_content_json(&supported_feedback.response);
    assert_eq!(supported_json["created"], true);
    assert_ne!(
        supported_json["feedbackPageId"],
        feedback_json["feedbackPageId"]
    );
    assert_eq!(
        supported_json["usedRevisionIds"],
        feedback_json["usedRevisionIds"]
    );
    assert_eq!(
        supported_json["evidenceRevisionIds"],
        json!([evidence_revision_id])
    );
    assert!(
        !supported_json["usedRevisionIds"]
            .as_array()
            .unwrap()
            .contains(&json!(evidence_revision_id))
    );

    let repeated_feedback = tools
        .execute_for_model(&correction_with_evidence, Some("test-model"), "interactive")
        .await;
    assert_eq!(repeated_feedback.response["success"], true);
    let repeated_json = tool_content_json(&repeated_feedback.response);
    assert_eq!(repeated_json["created"], false);
    assert_eq!(
        repeated_json["feedbackPageId"],
        supported_json["feedbackPageId"]
    );
    assert_eq!(
        repeated_json["evidenceRevisionIds"],
        json!([evidence_revision_id])
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
        ContinuityHost::open_embedded_for_test(store)
            .await
            .expect("open continuity host"),
    );
    let mut recalled = Vec::new();
    for content in [
        "An older conversation Revision can still support a new Episode.",
        "The user sustains that recalled design line in another turn.",
        "A third user-authored turn makes the line eligible as a Topic.",
    ] {
        recalled.push(
            continuity
                .ingest_message(
                    MemoryRole::User,
                    content,
                    Vec::new(),
                    None,
                    MessageLinks::default(),
                )
                .await
                .expect("ingest recalled conversation source"),
        );
    }
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

    let rejected = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "upsert_episode",
                "arguments": {
                    "title": "Premature design line",
                    "summary": "One recalled turn is not enough for the user-visible Topic sequence.",
                    "state": "forming",
                    "source_revision_ids": [recalled[0].page.revision_id.clone()]
                }
            }),
            Some("test-model"),
            "reflection",
        )
        .await;
    assert_eq!(rejected.response["success"], false);

    let result = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "upsert_episode",
                "arguments": {
                    "title": "Recalled design line",
                    "summary": "The source is outside Reflection's imported event tail but remains auditable through PCP.",
                    "state": "active",
                    "source_revision_ids": recalled.iter().map(|source| source.page.revision_id.clone()).collect::<Vec<_>>()
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
                    "source_revision_ids": [recalled[0].page.revision_id.clone()]
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
                    "source_revision_ids": [recalled[0].page.revision_id.clone()]
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

    let discussion = tools
        .execute_for_model(
            &json!({
                "namespace": "symbiont",
                "tool": "propose_proactive_message",
                "arguments": {
                    "message": "这件近期发生的事本身值得聊聊。",
                    "reason": "Community reaction has created a concrete tension.",
                    "kind": "discussion",
                    "source_revision_ids": []
                }
            }),
            Some("test-model"),
            "autonomous",
        )
        .await;
    assert_eq!(
        discussion.response["success"], true,
        "{}",
        discussion.response
    );

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
