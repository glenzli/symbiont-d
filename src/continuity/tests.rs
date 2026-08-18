use super::{CONVERSATION_NAMESPACE, PROJECT_NAMESPACE, ScopePolicy, SourceSequence};

#[tokio::test]
async fn source_sequence_is_durable_and_monotonic_across_reopen() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("pcp-source-sequence.json");

    let first = SourceSequence::open(path.clone())
        .await
        .expect("open source sequence");
    assert_eq!(first.reserve().await.expect("first sequence"), 1);
    assert_eq!(first.reserve().await.expect("second sequence"), 2);
    drop(first);

    let reopened = SourceSequence::open(path)
        .await
        .expect("reopen source sequence");
    assert_eq!(reopened.reserve().await.expect("resumed sequence"), 3);
}

#[test]
fn tenant_scope_policy_uses_the_canonical_user_self_scope() {
    let scopes = ScopePolicy::for_owner("idn_runtime_identity");
    assert_eq!(scopes.user, "user:self");
    assert_eq!(scopes.project, PROJECT_NAMESPACE);
    assert_eq!(scopes.conversation, CONVERSATION_NAMESPACE);
}
