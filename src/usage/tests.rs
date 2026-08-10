use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::Connection;
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
async fn migrates_legacy_origins_into_stable_activity_fields() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("symbiont-usage-activity-migration-{nonce}.sqlite3"));
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "
            CREATE TABLE invocations (
                id TEXT PRIMARY KEY, parent_id TEXT, thread_id TEXT NOT NULL,
                turn_id TEXT NOT NULL UNIQUE, origin TEXT NOT NULL, lane TEXT NOT NULL,
                requested_model TEXT NOT NULL, effective_model TEXT NOT NULL,
                model_display_name TEXT NOT NULL, effort TEXT NOT NULL, service_tier TEXT,
                started_at TEXT NOT NULL, completed_at TEXT NOT NULL, duration_ms INTEGER NOT NULL,
                status TEXT NOT NULL, input_tokens INTEGER NOT NULL,
                cached_input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                reasoning_output_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
                tool_calls_json TEXT NOT NULL, produced_message INTEGER NOT NULL
            );
            INSERT INTO invocations VALUES (
                'luna-run', NULL, 'thread', 'turn', 'luna_sense', 'sense',
                'luna', 'luna', 'Luna', 'low', NULL,
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:01Z', 1000,
                'completed', 1, 0, 1, 0, 2, '[]', 0
            );
            ",
        )
        .unwrap();
    drop(connection);

    UsageStore::open(path.clone()).await.unwrap();

    let connection = Connection::open(&path).unwrap();
    let projection = connection
        .query_row(
            "SELECT activity, stage, input_source FROM invocations WHERE id = 'luna-run'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        projection,
        (
            "sensing".to_owned(),
            "sense".to_owned(),
            Some("luna".to_owned())
        )
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
    let sensing_started =
        (started - Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let root_started = started.to_rfc3339_opts(SecondsFormat::Millis, true);
    let child_started =
        (started + Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let completed = (started + Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[
            autonomous_invocation(
                "sense-root",
                None,
                "ambient_sense",
                &sensing_started,
                &root_started,
                30,
                false,
                Vec::new(),
                Vec::new(),
            ),
            autonomous_invocation(
                "explore-root",
                Some("sense-root"),
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
                Some("sense-root"),
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
                        "kind": "discussion",
                        "source_revision_ids": []
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
    assert_eq!(runs[0].trace_id, "sense-root");
    assert_eq!(runs[0].scope, "exploration");
    assert_eq!(runs[0].model_runs.len(), 3);
    assert_eq!(runs[0].model_runs[0].stage, "sense");
    assert_eq!(runs[0].model_runs[1].stage, "scout");
    assert_eq!(runs[0].model_runs[2].stage, "review");
    assert_eq!(runs[0].total_tokens, 230);
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
        runs[0].outreach_kind,
        Some(crate::outreach::OutreachKind::Discussion)
    );
    assert_eq!(
        runs[0].message.as_deref(),
        Some("A signal worth surfacing.")
    );
    let headline = store.headline("2025-12-31T16:00:00Z").await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 230);
    assert_eq!(headline.autonomous_messages_today, 1);
    assert_eq!(headline.autonomous_notes_today, 1);
    assert_eq!(headline.autonomous_interventions_today, 0);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn keeps_a_terminal_sensing_pass_in_recent_exploration_history() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-sensing-history-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let started = Utc::now();
    let started_at = started.to_rfc3339_opts(SecondsFormat::Millis, true);
    let completed_at =
        (started + Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[autonomous_invocation(
            "sense-only",
            None,
            "luna_sense",
            &started_at,
            &completed_at,
            40,
            false,
            vec![ToolTraceStep {
                sequence: 0,
                namespace: "symbiont".to_owned(),
                tool: "fetch_url".to_owned(),
                started_at: started_at.clone(),
                completed_at: completed_at.clone(),
                duration_ms: 2_000,
                succeeded: false,
                arguments: json!({
                    "url": "https://example.com/release",
                    "purpose": "Check a recent release"
                }),
                result: json!({"error": "background policy"}),
            }],
            Vec::new(),
        )])
        .await
        .unwrap();

    let runs = store.recent_explorations(5).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trace_id, "sense-only");
    assert_eq!(runs[0].scope, "sensing");
    assert_eq!(runs[0].model_runs[0].stage, "sense");
    assert_eq!(runs[0].sensing_candidate_count, 0);
    assert_eq!(runs[0].web_searches, 1);
    assert_eq!(
        runs[0].focus.as_ref().map(|focus| focus.title.as_str()),
        Some("Check a recent release")
    );
    assert_eq!(
        store
            .latest_exploration_completed_at()
            .await
            .unwrap()
            .as_deref(),
        Some(completed_at.as_str())
    );
    let headline = store.headline(&started_at).await.unwrap();
    assert_eq!(headline.autonomous_tokens_today, 40);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn mailbox_review_reports_the_external_input_instead_of_internal_reasoning() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-mail-review-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let started = Utc::now();
    let started_at = started.to_rfc3339_opts(SecondsFormat::Millis, true);
    let completed_at =
        (started + Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[autonomous_invocation(
            "mail-review",
            None,
            "ambient_review",
            &started_at,
            &completed_at,
            40,
            false,
            vec![ToolTraceStep {
                sequence: 0,
                namespace: "symbiont".to_owned(),
                tool: "review_sensing_candidates".to_owned(),
                started_at: started_at.clone(),
                completed_at: completed_at.clone(),
                duration_ms: 2_000,
                succeeded: true,
                arguments: json!({
                    "decisions": [{
                        "candidate_id": "mail-1",
                        "disposition": "input",
                        "reason": "Interesting but attributed",
                        "presentation": "condensed",
                        "display_text": "A research digest reports a new solar observation."
                    }]
                }),
                result: json!({
                    "accepted": true,
                    "hostRouting": {
                        "publishedInputCount": 1,
                        "suppressedInputCount": 0,
                        "deferredCandidateCount": 0
                    }
                }),
            }],
            vec![ExecutionTraceEvent {
                sequence: 0,
                kind: TraceEventKind::ReasoningSummary,
                occurred_at: started_at.clone(),
                title: "Model reasoning summary".to_owned(),
                details: json!({"summary": ["Preparing tool call"]}),
            }],
        )])
        .await
        .unwrap();

    let runs = store.recent_explorations(5).await.unwrap();
    assert_eq!(runs[0].sensing_candidate_count, 1);
    assert_eq!(runs[0].sensing_input_count, 1);
    assert_eq!(runs[0].sensing_published_count, 1);
    assert_eq!(runs[0].sensing_suppressed_count, 0);
    assert_eq!(
        runs[0].focus.as_ref().map(|focus| focus.title.as_str()),
        Some("A research digest reports a new solar observation.")
    );

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn infer_runtime_review_reports_host_routing_without_a_codex_tool_call() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-runtime-review-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let started = Utc::now();
    let started_at = started.to_rfc3339_opts(SecondsFormat::Millis, true);
    let completed_at =
        (started + Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[autonomous_invocation(
            "runtime-review",
            None,
            "ambient_review",
            &started_at,
            &completed_at,
            25,
            false,
            vec![ToolTraceStep {
                sequence: 0,
                namespace: "symbiont".to_owned(),
                tool: "route_sensing_candidates".to_owned(),
                started_at: completed_at.clone(),
                completed_at: completed_at.clone(),
                duration_ms: 0,
                succeeded: true,
                arguments: json!({
                    "reviewedCandidateCount": 4,
                    "inputCount": 2,
                    "deepCount": 1,
                    "discardCount": 1
                }),
                result: json!({
                    "accepted": true,
                    "hostRouting": {
                        "publishedInputCount": 1,
                        "suppressedInputCount": 1,
                        "deferredCandidateCount": 0
                    }
                }),
            }],
            Vec::new(),
        )])
        .await
        .unwrap();

    let runs = store.recent_explorations(5).await.unwrap();
    assert!(runs[0].sensing_reviewed);
    assert_eq!(runs[0].sensing_candidate_count, 4);
    assert_eq!(runs[0].sensing_input_count, 2);
    assert_eq!(runs[0].sensing_deep_count, 1);
    assert_eq!(runs[0].sensing_discard_count, 1);
    assert_eq!(runs[0].sensing_published_count, 1);
    assert_eq!(runs[0].sensing_suppressed_count, 1);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn does_not_duplicate_legacy_sensing_before_a_full_exploration() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-legacy-sensing-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
    let started = Utc::now();
    let sense_started = started.to_rfc3339_opts(SecondsFormat::Millis, true);
    let sense_completed =
        (started + Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let scout_completed =
        (started + Duration::seconds(3)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .record_all(&[
            autonomous_invocation(
                "legacy-sense",
                None,
                "ambient_sense",
                &sense_started,
                &sense_completed,
                20,
                false,
                Vec::new(),
                Vec::new(),
            ),
            autonomous_invocation(
                "legacy-scout",
                None,
                "autonomous_scout",
                &sense_completed,
                &scout_completed,
                80,
                false,
                Vec::new(),
                Vec::new(),
            ),
        ])
        .await
        .unwrap();

    let runs = store.recent_explorations(5).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trace_id, "legacy-scout");
    assert_eq!(runs[0].scope, "exploration");

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
