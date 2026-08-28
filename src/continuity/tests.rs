use super::{PCP_NAMESPACE, ScopePolicy, SourceSequence};

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
fn tenant_scope_policy_uses_only_the_symbiont_scope() {
    let scopes = ScopePolicy::for_owner("idn_runtime_identity");
    assert_eq!(scopes.namespace, PCP_NAMESPACE);
    assert_eq!(scopes.all(), vec![PCP_NAMESPACE.to_owned()]);
}
