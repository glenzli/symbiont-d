use std::sync::{Arc, atomic::Ordering};

use super::super::{
    Attribution, Disposition,
    tests::{Fixture, decision, proposal},
};
use super::*;
use pcp_client::{
    AccessMode,
    context_hub::{ClientContextPolicy, ContextHubRequest, ContextHubService},
};
use pcp_core::{AccessPrincipal, AccessPrincipalType, AccessSession};

fn operator() -> AccessSession {
    AccessMode::Admin.store_wide_session(
        AccessPrincipal {
            principal_id: "operator:test".into(),
            principal_type: AccessPrincipalType::Service,
            display_name: None,
        },
        "test",
        vec![],
        true,
    )
}
async fn enable(f: &Fixture) {
    f.hub
        .execute(
            &operator(),
            ContextHubRequest::SetPolicy(ClientContextPolicy {
                client_id: f.host.store.access().principal.principal_id.clone(),
                submit_candidates: true,
                publish_activity: false,
                read_activity: false,
            }),
        )
        .await
        .unwrap();
}
async fn inspect(f: &Fixture) -> Value {
    f.hub
        .execute(&operator(), ContextHubRequest::Inspect)
        .await
        .unwrap()
}
async fn reviewed(f: &Fixture) -> (Proposal, RetentionReview) {
    let source = f.source(&super::super::timestamp(), crate::memory::MemoryRole::User,
        "A source-backed trial showed retries shared an event identity, while the eventual real-world outcome remains unknown.").await;
    let proposal = proposal(
        &source,
        "A bounded trial preserved an unchanged event identity across retries; the broader result is not established.",
    );
    let packet = f.host.retain_page(proposal.clone(), None).await.unwrap();
    let mut review = decision(&packet, Disposition::NewSubject, vec![]);
    review.retention_basis = Some(RetentionBasis::ConcreteEvidence);
    review.attribution = Attribution::Mixed;
    review.rationale =
        "This single trial is useful evidence, but does not establish a universal retry strategy."
            .into();
    (proposal, review)
}

#[tokio::test]
async fn native_checkpoint_is_opt_in_and_does_not_capture_ordinary_explicit_state() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = false;
    let (proposal, review) = reviewed(&f).await;
    assert_eq!(
        f.host.retain_page(proposal, Some(review)).await.unwrap()["status"],
        "written"
    );
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 0);
    assert!(!f.host.experience_path().exists());

    f.host.experience_enabled = true;
    let (proposal, mut review) = reviewed(&f).await;
    review.retention_basis = Some(RetentionBasis::ExplicitState);
    let result = f.host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(result["status"], "written");
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_publication_retains_structured_provenance_and_exact_receipt_without_new_model() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = true;
    let (proposal, review) = reviewed(&f).await;
    let result = f
        .host
        .retain_page(proposal.clone(), Some(review.clone()))
        .await
        .unwrap();
    assert_eq!(result["status"], "written");
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 1);
    let inbox = inspect(&f).await;
    let items = inbox["candidates"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["experience"]["interpretation"], review.rationale);
    assert!(
        item["experience"]["observation"]
            .as_str()
            .unwrap()
            .contains("does not independently validate")
    );
    assert_eq!(item["experience"]["receipts"][0]["stage"], "publication");
    assert_eq!(
        item["experience"]["receipts"][0]["version"],
        result["revisionId"]
    );
    assert!(
        item["input"]["basedOnRevisionIds"]
            .as_array()
            .unwrap()
            .contains(&result["revisionId"])
    );
    assert_eq!(item["input"]["sourceRefs"].as_array().unwrap().len(), 2);
    let archive: Value =
        serde_json::from_slice(&std::fs::read(&f.host.retention.path).unwrap()).unwrap();
    let id = result["proposalId"].as_str().unwrap();
    let receipts = archive["proposals"][id]["experienceReceipts"]
        .as_object()
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts.values().next().unwrap(), &result);
    assert_eq!(
        f.count().await,
        1,
        "experience staging does not create another Page"
    );
    f.host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(
        f.delivery_calls.load(Ordering::SeqCst),
        1,
        "receipt reuse neither recaptures nor replays delivered evidence"
    );
    assert_eq!(
        f.host.retention_snapshot().await["experience"]["pending"],
        0
    );
}

#[tokio::test]
async fn offline_then_restart_then_unknown_ack_reuses_one_exact_event() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = true;
    let (proposal, review) = reviewed(&f).await;
    f.delivery_mode.store(1, Ordering::SeqCst);
    f.host
        .retain_page(proposal.clone(), Some(review.clone()))
        .await
        .unwrap();
    assert_eq!(
        f.host.retention_snapshot().await["experience"]["pending"],
        1
    );
    let pending = std::fs::read_dir(f.host.experience_path())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().is_some_and(|ext| ext == "json")).then_some(path)
        })
        .next()
        .unwrap();
    let before = std::fs::read(&pending).unwrap();
    let store = Arc::clone(&f.host.store);
    let mut host = ContinuityHost::open(store, f.transcript.clone())
        .await
        .unwrap();
    host.experience_enabled = true;
    f.delivery_mode.store(2, Ordering::SeqCst);
    host.retain_page(proposal.clone(), Some(review.clone()))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&pending).unwrap(),
        before,
        "unknown ACK retains exact bytes and original age"
    );
    assert_eq!(inspect(&f).await["candidates"].as_array().unwrap().len(), 1);
    f.delivery_mode.store(0, Ordering::SeqCst);
    host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(inspect(&f).await["candidates"].as_array().unwrap().len(), 1);
    let status = host.retention_snapshot().await;
    assert_eq!(
        status["experience"]["delivery"]["lastReceipt"]["created"],
        false
    );
    assert_eq!(status["experience"]["pending"], 0);
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn runtime_opt_in_denial_is_durable_and_does_not_retry_at_future_checkpoints() {
    let mut f = Fixture::new(false).await;
    f.host.experience_enabled = true;
    let (proposal, review) = reviewed(&f).await;
    assert_eq!(
        f.host
            .retain_page(proposal.clone(), Some(review.clone()))
            .await
            .unwrap()["status"],
        "written"
    );
    assert_eq!(
        f.host.retention_snapshot().await["experience"]["delivery"]["paused"],
        true
    );
    enable(&f).await;
    let mut host = ContinuityHost::open(Arc::clone(&f.host.store), f.transcript.clone())
        .await
        .unwrap();
    host.experience_enabled = true;
    host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.retention_snapshot().await["experience"]["pending"], 1);
}

#[tokio::test]
async fn deferred_review_keeps_uncertainty_and_a_native_validation_receipt() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = true;
    let (proposal, mut review) = reviewed(&f).await;
    review.disposition = Disposition::Defer;
    review.rationale =
        "The source-backed trial exists, but comparison evidence is temporarily unavailable."
            .into();
    assert_eq!(
        f.host.retain_page(proposal, Some(review)).await.unwrap()["status"],
        "deferred"
    );
    let inbox = inspect(&f).await;
    let experience = &inbox["candidates"][0]["experience"];
    assert_eq!(experience["receipts"][0]["stage"], "validation");
    assert_eq!(experience["receipts"][0]["outcome"], "unknown");
    assert_eq!(experience["unresolved"].as_array().unwrap().len(), 1);
    assert_eq!(f.count().await, 0);
}

#[tokio::test]
async fn changed_principal_or_scope_holds_native_capture_without_dispatch() {
    let mut f = Fixture::new(false).await;
    f.host.experience_enabled = true;
    let (proposal, review) = reviewed(&f).await;
    // Capture is atomic with the receipt even if staging has not happened yet.
    f.host
        .retain_page_review(proposal.clone(), Some(review))
        .await
        .unwrap();
    let mut state = f.host.retention.state.lock().await;
    let record = state
        .proposals
        .values_mut()
        .find(|record| record.experience.is_some())
        .unwrap();
    record.experience.as_mut().unwrap().principal_id = "other:principal".into();
    f.host.retention.save(&state).await.unwrap();
    drop(state);
    f.host.retention_experience_checkpoint().await.unwrap();
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.host.retention_snapshot().await["experience"]["foreignHeld"],
        1
    );
    let mut state = f.host.retention.state.lock().await;
    let item = state
        .proposals
        .values_mut()
        .find_map(|record| record.experience.as_mut())
        .unwrap();
    item.principal_id = f.host.store.access().principal.principal_id.clone();
    item.input.candidate.scope = "foreign:scope".into();
    drop(state);
    f.host.retention_experience_checkpoint().await.unwrap();
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expired_pending_request_is_visible_and_never_sent() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = true;
    f.delivery_mode.store(1, Ordering::SeqCst);
    let (proposal, review) = reviewed(&f).await;
    f.host
        .retain_page(proposal.clone(), Some(review.clone()))
        .await
        .unwrap();
    let path = std::fs::read_dir(f.host.experience_path())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().is_some_and(|ext| ext == "json")).then_some(path)
        })
        .next()
        .unwrap();
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["stagedAt"] = json!(1);
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    f.host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 1);
    let status = f.host.retention_snapshot().await;
    assert_eq!(status["experience"]["delivery"]["paused"], true);
    assert_eq!(status["experience"]["pending"], 1);
}

#[tokio::test]
async fn full_outbox_declines_new_capture_without_losing_existing_evidence_or_native_publication() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = true;
    f.delivery_mode.store(1, Ordering::SeqCst);
    let (proposal, review) = reviewed(&f).await;
    let outbox = ExperienceOutbox::open(
        f.host.experience_path(),
        f.host.store.identity_id().into(),
        f.host.store.access().principal.principal_id.clone(),
    )
    .unwrap();
    for index in 0..MAX_PENDING {
        outbox
            .stage(
                Experience {
                    topic_key: Some(format!("fixture-{index}")),
                    conditions: None,
                    attempt: "A bounded retained attempt".into(),
                    observation: "An observed outcome".into(),
                    interpretation: None,
                    unresolved: vec![],
                    receipts: vec![],
                }
                .into_candidate(f.host.pcp_scope().into(), "Prior evidence".into())
                .unwrap(),
            )
            .unwrap();
    }
    let before = std::fs::read_dir(f.host.experience_path())
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), std::fs::read(&path).unwrap())
        })
        .collect::<Vec<_>>();
    let result = f.host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(result["status"], "written");
    let snapshot = f.host.retention_snapshot().await;
    assert_eq!(snapshot["experience"]["pending"], MAX_PENDING);
    assert_eq!(
        snapshot["experience"]["delivery"]["lastCapture"]["status"],
        "declined"
    );
    for (path, bytes) in before {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn crash_after_staging_before_native_save_deduplicates_and_delivers_once() {
    let mut f = Fixture::new(false).await;
    enable(&f).await;
    f.host.experience_enabled = true;
    let (proposal, review) = reviewed(&f).await;
    f.host
        .retain_page_review(proposal.clone(), Some(review.clone()))
        .await
        .unwrap();
    let state = f.host.retention.state.lock().await;
    let item = state
        .proposals
        .values()
        .find_map(|record| record.experience.as_ref())
        .unwrap();
    let outbox = ExperienceOutbox::open(
        f.host.experience_path(),
        item.identity_id.clone(),
        item.principal_id.clone(),
    )
    .unwrap();
    outbox.stage(item.input.clone()).unwrap();
    drop(state);
    let mut host = ContinuityHost::open(Arc::clone(&f.host.store), f.transcript.clone())
        .await
        .unwrap();
    host.experience_enabled = true;
    host.retain_page(proposal, Some(review)).await.unwrap();
    assert_eq!(inspect(&f).await["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(f.delivery_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.retention_snapshot().await["experience"]["pending"], 0);
}
