use super::{PCP_NAMESPACE, ScopePolicy, SourceSequence};
use pcp_client::AccessMode;
use pcp_core::{AccessPrincipal, AccessPrincipalType};

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

#[test]
fn approved_cross_host_scopes_are_readable_without_gaining_write_access() {
    let principal = AccessPrincipal {
        principal_id: "host:symbiont-d".to_owned(),
        principal_type: AccessPrincipalType::Host,
        display_name: Some("Symbiont".to_owned()),
    };
    let mut access = AccessMode::Contribute.session(
        principal.clone(),
        "session",
        vec![PCP_NAMESPACE.to_owned()],
        false,
    );
    access.grants.extend(
        AccessMode::Read
            .session(
                principal,
                "session",
                vec!["drive".to_owned(), "codex".to_owned()],
                false,
            )
            .grants,
    );

    let policy = ScopePolicy::for_access(&access).expect("cross-Host scope policy");
    assert_eq!(
        policy.all(),
        vec![
            "codex".to_owned(),
            "drive".to_owned(),
            PCP_NAMESPACE.to_owned()
        ]
    );
    assert!(
        access.allows("drive", pcp_core::AccessPermission::ReadDetail)
            && !access.allows("drive", pcp_core::AccessPermission::Ingest)
    );
}
