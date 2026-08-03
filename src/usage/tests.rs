use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, SecondsFormat, Utc};
use serde_json::json;

use crate::diagnostics::{
    ContextFragment, ContextSnapshot, ExecutionTraceEvent, NativeThreadSnapshot, TraceEventKind,
};

use super::{InvocationRecord, ToolTraceStep, UsageStore};

#[tokio::test]
async fn records_and_groups_invocations() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-usage-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[InvocationRecord {
            id: "turn-1".to_owned(),
            parent_id: None,
            thread_id: "thread".to_owned(),
            turn_id: "turn-1".to_owned(),
            origin: "interactive".to_owned(),
            lane: "conversation".to_owned(),
            requested_model: "gpt-test".to_owned(),
            effective_model: "gpt-test".to_owned(),
            model_display_name: "GPT Test".to_owned(),
            effort: "medium".to_owned(),
            service_tier: None,
            started_at: timestamp.clone(),
            completed_at: timestamp.clone(),
            duration_ms: 1_000,
            status: "completed".to_owned(),
            input_tokens: 90,
            cached_input_tokens: 50,
            output_tokens: 10,
            reasoning_output_tokens: 2,
            total_tokens: 100,
            tool_calls: vec!["pcp.search_pages".to_owned()],
            produced_message: true,
            trace_steps: vec![
                ToolTraceStep {
                    sequence: 0,
                    namespace: "pcp".to_owned(),
                    tool: "search_pages".to_owned(),
                    started_at: "2026-01-01T00:00:00Z".to_owned(),
                    completed_at: "2026-01-01T00:00:00.010Z".to_owned(),
                    duration_ms: 10,
                    succeeded: true,
                    arguments: json!({"query": "context"}),
                    result: json!({"success": true}),
                },
                ToolTraceStep {
                    sequence: 1,
                    namespace: "pcp".to_owned(),
                    tool: "write_page".to_owned(),
                    started_at: "2026-01-01T00:00:00.010Z".to_owned(),
                    completed_at: "2026-01-01T00:00:00.020Z".to_owned(),
                    duration_ms: 10,
                    succeeded: true,
                    arguments: json!({"content": "Remember this."}),
                    result: json!({"success": true}),
                },
            ],
            context_snapshot: Some(ContextSnapshot {
                input: vec![json!({"type": "text", "text": "hello"})],
                fragments: vec![ContextFragment {
                    source: "symbiont.pcp".to_owned(),
                    kind: "application".to_owned(),
                    value: "PCP is available.".to_owned(),
                }],
                working_context: None,
                developer_instructions: "test instructions".to_owned(),
                native_thread: NativeThreadSnapshot {
                    thread_id: "thread".to_owned(),
                    cursor_before: None,
                    prior_turns: 0,
                    compactions_before: 0,
                    model_context_window: Some(128_000),
                    exact_prompt_available: false,
                    observable_history_tail: Vec::new(),
                    history_tail_truncated: false,
                },
            }),
            trace_events: vec![ExecutionTraceEvent {
                sequence: 0,
                kind: TraceEventKind::ReasoningSummary,
                occurred_at: timestamp,
                title: "Model reasoning summary".to_owned(),
                details: json!({"summary": ["Look up context."]}),
            }],
        }])
        .await
        .unwrap();

    let summary = store.summary().await.unwrap();
    assert_eq!(summary.totals.invocations, 1);
    assert_eq!(summary.totals.total_tokens, 100);
    assert_eq!(summary.by_model[0].display_name, "GPT Test");
    assert_eq!(summary.recent[0].tool_calls, vec!["pcp.search_pages"]);

    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.total_tokens, 100);
    assert_eq!(headline.autonomous_tokens_today, 0);

    let trace = store.trace("turn-1").await.unwrap().unwrap();
    assert_eq!(trace.pcp_tool_calls, 2);
    assert_eq!(trace.pcp_recall_calls, 1);
    assert_eq!(trace.pcp_write_calls, 1);
    assert!(trace.details_retained);
    assert_eq!(trace.event_count, 1);
    assert_eq!(trace.runs[0].steps[0].arguments["query"], "context");
    assert_eq!(
        trace.runs[0]
            .context
            .as_ref()
            .unwrap()
            .native_thread
            .prior_turns,
        0
    );
    assert_eq!(
        trace.runs[0].events[0].details["summary"][0],
        "Look up context."
    );

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn background_budget_includes_memory_maintenance_without_counting_a_message() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-maintenance-usage-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut invocation = autonomous_invocation(
        "maintenance-1",
        None,
        "autonomous",
        &timestamp,
        &timestamp,
        42,
        false,
        Vec::new(),
        Vec::new(),
    );
    invocation.origin = "maintenance".to_owned();
    store.record_all(&[invocation]).await.unwrap();

    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 42);
    assert_eq!(headline.autonomous_messages_today, 0);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn reconciliation_counts_toward_both_background_budgets() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-reconciliation-usage-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut invocation = autonomous_invocation(
        "reconciliation-preview",
        None,
        "autonomous",
        &timestamp,
        &timestamp,
        42,
        false,
        Vec::new(),
        Vec::new(),
    );
    invocation.origin = "reconciliation_preview".to_owned();
    store.record_all(&[invocation]).await.unwrap();

    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 42);
    assert_eq!(headline.reflection_tokens_today, 42);
    assert_eq!(headline.autonomous_messages_today, 0);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn pcp_runtime_observation_counts_toward_both_background_budgets() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-pcp-observe-usage-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut invocation = autonomous_invocation(
        "pcp-observe",
        None,
        "autonomous",
        &timestamp,
        &timestamp,
        42,
        false,
        Vec::new(),
        Vec::new(),
    );
    invocation.origin = "pcp_maintenance".to_owned();
    store.record_all(&[invocation]).await.unwrap();

    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 42);
    assert_eq!(headline.reflection_tokens_today, 42);
    assert_eq!(headline.autonomous_messages_today, 0);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn reflection_outreach_counts_as_an_attention_interruption_not_exploration_tokens() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-reflection-usage-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut invocation = autonomous_invocation(
        "reflection-outreach",
        None,
        "autonomous",
        &timestamp,
        &timestamp,
        42,
        true,
        Vec::new(),
        Vec::new(),
    );
    invocation.origin = "reflection".to_owned();
    store.record_all(&[invocation]).await.unwrap();

    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 0);
    assert_eq!(headline.autonomous_messages_today, 1);
    assert_eq!(headline.autonomous_interventions_today, 1);
    assert_eq!(headline.autonomous_notes_today, 0);
    assert_eq!(headline.reflection_tokens_today, 42);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn prunes_expired_details_without_losing_usage_history() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-retention-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    store
        .record_all(&[InvocationRecord {
            id: "old-turn".to_owned(),
            parent_id: None,
            thread_id: "thread".to_owned(),
            turn_id: "old-turn".to_owned(),
            origin: "interactive".to_owned(),
            lane: "conversation".to_owned(),
            requested_model: "gpt-test".to_owned(),
            effective_model: "gpt-test".to_owned(),
            model_display_name: "GPT Test".to_owned(),
            effort: "medium".to_owned(),
            service_tier: None,
            started_at: "2020-01-01T00:00:00Z".to_owned(),
            completed_at: "2020-01-01T00:00:01Z".to_owned(),
            duration_ms: 1_000,
            status: "completed".to_owned(),
            input_tokens: 5,
            cached_input_tokens: 0,
            output_tokens: 5,
            reasoning_output_tokens: 0,
            total_tokens: 10,
            tool_calls: vec!["pcp.search_pages".to_owned()],
            produced_message: true,
            trace_steps: vec![ToolTraceStep {
                sequence: 0,
                namespace: "pcp".to_owned(),
                tool: "search_pages".to_owned(),
                started_at: "2020-01-01T00:00:00Z".to_owned(),
                completed_at: "2020-01-01T00:00:00.010Z".to_owned(),
                duration_ms: 10,
                succeeded: true,
                arguments: json!({"query": "old"}),
                result: json!({"success": true}),
            }],
            context_snapshot: None,
            trace_events: Vec::new(),
        }])
        .await
        .unwrap();

    let summary = store.summary().await.unwrap();
    assert_eq!(summary.totals.invocations, 1);
    assert_eq!(summary.totals.total_tokens, 10);
    let trace = store.trace("old-turn").await.unwrap().unwrap();
    assert!(!trace.details_retained);
    assert!(trace.runs[0].steps.is_empty());

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn groups_recent_autonomous_runs_into_exploration_cycles() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-exploration-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let started = Utc::now();
    let root_started = started.to_rfc3339_opts(SecondsFormat::Millis, true);
    let child_started =
        (started + Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let completed = (started + Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[
            autonomous_invocation(
                "explore-root",
                None,
                "autonomous_scout",
                &root_started,
                &child_started,
                120,
                false,
                vec![
                    ToolTraceStep {
                        sequence: 0,
                        namespace: "pcp".to_owned(),
                        tool: "read_pages".to_owned(),
                        started_at: root_started.clone(),
                        completed_at: child_started.clone(),
                        duration_ms: 1_000,
                        succeeded: true,
                        arguments: json!({"revisionIds": ["context"]}),
                        result: json!({"success": true}),
                    },
                    ToolTraceStep {
                        sequence: 1,
                        namespace: "symbiont".to_owned(),
                        tool: "submit_exploration_finding".to_owned(),
                        started_at: root_started.clone(),
                        completed_at: child_started.clone(),
                        duration_ms: 1_000,
                        succeeded: true,
                        arguments: json!({
                            "topic": "Agent runtime durability",
                            "claim": "Recovery semantics matter more than static recall.",
                        }),
                        result: json!({"success": true}),
                    },
                ],
                vec![
                    ExecutionTraceEvent {
                        sequence: 0,
                        kind: TraceEventKind::ReasoningSummary,
                        occurred_at: root_started.clone(),
                        title: "Model reasoning summary".to_owned(),
                        details: json!({"summary": ["Check durable context."]}),
                    },
                    ExecutionTraceEvent {
                        sequence: 1,
                        kind: TraceEventKind::WebSearch,
                        occurred_at: child_started.clone(),
                        title: "Live web search".to_owned(),
                        details: json!({"query": "current signal"}),
                    },
                ],
            ),
            autonomous_invocation(
                "explore-child",
                Some("explore-root"),
                "autonomous",
                &child_started,
                &completed,
                80,
                true,
                vec![ToolTraceStep {
                    sequence: 0,
                    namespace: "symbiont".to_owned(),
                    tool: "propose_proactive_message".to_owned(),
                    started_at: child_started.clone(),
                    completed_at: completed.clone(),
                    duration_ms: 1_000,
                    succeeded: true,
                    arguments: json!({
                        "message": "A signal worth surfacing.",
                        "reason": "It changes the current decision.",
                        "kind": "note",
                        "source_revision_ids": ["rev_0123456789abcdef0123456789abcdef"]
                    }),
                    result: json!({"success": true}),
                }],
                vec![ExecutionTraceEvent {
                    sequence: 0,
                    kind: TraceEventKind::AgentMessage,
                    occurred_at: completed.clone(),
                    title: "Final assistant message".to_owned(),
                    details: json!({"text": "<symbiont-silent/>"}),
                }],
            ),
        ])
        .await
        .unwrap();

    let runs = store.recent_explorations(5).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trace_id, "explore-root");
    assert_eq!(runs[0].model_runs.len(), 2);
    assert_eq!(runs[0].model_runs[0].stage, "scout");
    assert_eq!(runs[0].model_runs[1].stage, "review");
    assert_eq!(runs[0].total_tokens, 200);
    assert_eq!(runs[0].pcp_recall_calls, 1);
    assert_eq!(runs[0].web_searches, 1);
    assert_eq!(runs[0].search_queries, vec!["current signal"]);
    assert_eq!(
        runs[0].focus.as_ref().unwrap().title,
        "Agent runtime durability"
    );
    assert_eq!(
        runs[0].focus.as_ref().unwrap().detail.as_deref(),
        Some("Recovery semantics matter more than static recall.")
    );
    assert!(runs[0].surfaced);
    assert_eq!(
        runs[0].message.as_deref(),
        Some("A signal worth surfacing.")
    );
    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 200);
    assert_eq!(headline.autonomous_messages_today, 1);
    assert_eq!(headline.autonomous_notes_today, 1);
    assert_eq!(headline.autonomous_interventions_today, 0);

    std::fs::remove_file(path).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn autonomous_invocation(
    id: &str,
    parent_id: Option<&str>,
    origin: &str,
    started_at: &str,
    completed_at: &str,
    total_tokens: u64,
    produced_message: bool,
    trace_steps: Vec<ToolTraceStep>,
    trace_events: Vec<ExecutionTraceEvent>,
) -> InvocationRecord {
    InvocationRecord {
        id: id.to_owned(),
        parent_id: parent_id.map(str::to_owned),
        thread_id: "autonomous-thread".to_owned(),
        turn_id: id.to_owned(),
        origin: origin.to_owned(),
        lane: "observe".to_owned(),
        requested_model: "gpt-test".to_owned(),
        effective_model: "gpt-test".to_owned(),
        model_display_name: "GPT Test".to_owned(),
        effort: "medium".to_owned(),
        service_tier: None,
        started_at: started_at.to_owned(),
        completed_at: completed_at.to_owned(),
        duration_ms: 1_000,
        status: "completed".to_owned(),
        input_tokens: total_tokens.saturating_sub(10),
        cached_input_tokens: 0,
        output_tokens: 10,
        reasoning_output_tokens: 2,
        total_tokens,
        tool_calls: trace_steps
            .iter()
            .map(|step| format!("{}.{}", step.namespace, step.tool))
            .collect(),
        produced_message,
        trace_steps,
        context_snapshot: None,
        trace_events,
    }
}
