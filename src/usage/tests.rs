use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{InvocationRecord, ToolTraceStep, UsageStore};

#[tokio::test]
async fn records_and_groups_invocations() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("symbiont-usage-{nonce}.sqlite3"));
    let store = UsageStore::open(path.clone()).await.unwrap();
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
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            completed_at: "2026-01-01T00:00:01Z".to_owned(),
            duration_ms: 1_000,
            status: "completed".to_owned(),
            input_tokens: 90,
            cached_input_tokens: 50,
            output_tokens: 10,
            reasoning_output_tokens: 2,
            total_tokens: 100,
            tool_calls: vec!["pcp.search_pages".to_owned()],
            produced_message: true,
            trace_steps: vec![ToolTraceStep {
                sequence: 0,
                namespace: "pcp".to_owned(),
                tool: "search_pages".to_owned(),
                started_at: "2026-01-01T00:00:00Z".to_owned(),
                completed_at: "2026-01-01T00:00:00.010Z".to_owned(),
                duration_ms: 10,
                succeeded: true,
                arguments: json!({"query": "context"}),
                result: json!({"success": true}),
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
    assert_eq!(trace.pcp_tool_calls, 1);
    assert_eq!(trace.runs[0].steps[0].arguments["query"], "context");

    std::fs::remove_file(path).unwrap();
}
