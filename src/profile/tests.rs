use std::path::PathBuf;

use super::{CalibrationMode, ProfileStore, SetupStatus};

fn paths(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("symbiont-profile-{name}-{}", std::process::id()));
    (root.join("profile.toml"), root.join("orientation.md"))
}

#[tokio::test]
async fn initialization_is_explicit_and_independent_from_orientation_content() {
    let (state_path, orientation_path) = paths("explicit");
    let store = ProfileStore::open(state_path.clone(), orientation_path.clone())
        .await
        .expect("open profile");
    assert_eq!(store.snapshot().await.status, SetupStatus::Unconfigured);

    store
        .begin(CalibrationMode::Description)
        .await
        .expect("begin calibration");
    let ready = store
        .complete("# Current Context\n\nBuilding symbiont-d.")
        .await
        .expect("complete calibration");
    assert_eq!(ready.status, SetupStatus::Ready);

    let reopened = ProfileStore::open(state_path.clone(), orientation_path.clone())
        .await
        .expect("reopen profile");
    assert_eq!(reopened.snapshot().await.status, SetupStatus::Ready);
    assert!(
        reopened
            .snapshot()
            .await
            .orientation
            .contains("Building symbiont-d")
    );

    let _ = tokio::fs::remove_dir_all(state_path.parent().expect("test root")).await;
}
