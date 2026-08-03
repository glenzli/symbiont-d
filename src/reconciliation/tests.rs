use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{
    ReconciliationMode, ReconciliationProposal, ReconciliationProposalKind, ReconciliationStore,
    store::CompletedRun,
};

fn temp_path(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("symbiont-reconciliation-{label}-{nonce}.json"))
}

#[test]
fn accepts_tool_style_revision_ids_and_serializes_for_the_ui() {
    let proposal: ReconciliationProposal = serde_json::from_value(json!({
        "action": "synthesize",
        "subject": "PCP retrieval design",
        "reason": "Several durable Pages now form one reusable line.",
        "revision_ids": ["rev_a", "rev_b"]
    }))
    .expect("parse tool proposal");
    assert!(matches!(
        proposal.action,
        ReconciliationProposalKind::Synthesize
    ));
    assert_eq!(proposal.revision_ids, vec!["rev_a", "rev_b"]);
    let serialized = serde_json::to_value(proposal).expect("serialize proposal");
    assert_eq!(serialized["revisionIds"][0], "rev_a");
}

#[test]
fn accepts_model_proposed_page_consolidation() {
    let proposal: ReconciliationProposal = serde_json::from_value(json!({
        "action": "consolidate",
        "subject": "Repeated runtime design notes",
        "reason": "The current Pages restate one durable decision and can be replaced safely.",
        "revision_ids": ["rev_a", "rev_b", "rev_c"]
    }))
    .expect("parse consolidation proposal");
    assert!(matches!(
        proposal.action,
        ReconciliationProposalKind::Consolidate
    ));
    assert_eq!(proposal.revision_ids.len(), 3);
}

#[tokio::test]
async fn persists_preview_and_recovers_interrupted_runs() {
    let path = temp_path("store");
    let store = ReconciliationStore::open(path.clone()).await.unwrap();
    let preview_id = store
        .start_run(
            ReconciliationMode::Preview,
            "manual",
            "digest-a".to_owned(),
            4,
            None,
        )
        .await
        .unwrap();
    store
        .complete_run(
            &preview_id,
            CompletedRun {
                summary: Some("One structural change is worth reviewing.".to_owned()),
                proposals: vec![ReconciliationProposal {
                    action: ReconciliationProposalKind::Classify,
                    subject: "Durable protocol note".to_owned(),
                    reason: "Its role is stable and currently untyped.".to_owned(),
                    revision_ids: vec!["rev_note".to_owned()],
                }],
                actions: Vec::new(),
                trace_id: Some("trace-preview".to_owned()),
                model: Some("test-model".to_owned()),
                total_tokens: 120,
            },
        )
        .await
        .unwrap();
    let apply_id = store
        .start_run(
            ReconciliationMode::Apply,
            "manual",
            "digest-a".to_owned(),
            4,
            Some(preview_id.clone()),
        )
        .await
        .unwrap();
    drop(store);

    let reopened = ReconciliationStore::open(path.clone()).await.unwrap();
    assert_eq!(reopened.latest_preview().await.unwrap().id, preview_id);
    let interrupted = reopened.run(&apply_id).await.unwrap();
    assert_eq!(interrupted.status, "interrupted");
    assert_eq!(interrupted.error.as_deref(), Some("service_restarted"));

    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn runtime_observations_do_not_replace_the_user_reviewable_preview() {
    let path = temp_path("runtime-observation");
    let store = ReconciliationStore::open(path.clone()).await.unwrap();
    let manual_id = store
        .start_run(
            ReconciliationMode::Preview,
            "manual",
            "manual-digest".to_owned(),
            2,
            None,
        )
        .await
        .unwrap();
    store
        .complete_run(
            &manual_id,
            CompletedRun {
                summary: Some("Manual preview".to_owned()),
                proposals: Vec::new(),
                actions: Vec::new(),
                trace_id: None,
                model: None,
                total_tokens: 0,
            },
        )
        .await
        .unwrap();
    let runtime_id = store
        .start_run(
            ReconciliationMode::Preview,
            "pcp_runtime",
            "runtime-digest".to_owned(),
            12,
            None,
        )
        .await
        .unwrap();
    store
        .complete_run(
            &runtime_id,
            CompletedRun {
                summary: Some("Runtime observation".to_owned()),
                proposals: Vec::new(),
                actions: Vec::new(),
                trace_id: None,
                model: None,
                total_tokens: 0,
            },
        )
        .await
        .unwrap();

    assert_eq!(store.latest_preview().await.unwrap().id, manual_id);
    assert_eq!(store.recent_runs().await[0].id, runtime_id);

    let _ = tokio::fs::remove_file(path).await;
}
