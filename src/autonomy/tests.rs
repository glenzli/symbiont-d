use super::{AutonomyConfig, AutonomyStore};

#[tokio::test]
async fn autonomy_requires_both_user_permission_and_initialization() {
    let path = std::env::temp_dir().join(format!("symbiont-autonomy-{}.toml", std::process::id()));
    let store = AutonomyStore::open(path.clone())
        .await
        .expect("open autonomy config");
    assert!(!store.permitted(true).await);

    let mut config = AutonomyConfig::default();
    config.enabled = true;
    store.update(config).await.expect("enable autonomy");
    assert!(!store.permitted(false).await);
    assert!(store.permitted(true).await);

    let _ = tokio::fs::remove_file(path).await;
}
