use serde_json::json;

use super::{ComputeConfig, ComputeLane, ComputeStore, ModelInfo};

fn model(slug: &str, default: bool, efforts: &[&str]) -> ModelInfo {
    ModelInfo::from_app_server(&json!({
        "id": slug,
        "model": slug,
        "displayName": slug,
        "description": "test",
        "isDefault": default,
        "defaultReasoningEffort": efforts[0],
        "supportedReasoningEfforts": efforts.iter().map(|effort| json!({
            "reasoningEffort": effort,
            "description": effort
        })).collect::<Vec<_>>(),
        "serviceTiers": [],
        "inputModalities": ["text", "image"]
    }))
    .unwrap()
}

#[test]
fn defaults_choose_semantic_model_lanes() {
    let catalog = vec![
        model("gpt-5.4-mini", false, &["low", "medium"]),
        model("gpt-5.6-sol", false, &["medium", "high", "xhigh"]),
        model("gpt-5.6-terra", true, &["low", "medium", "high"]),
        model("gpt-5.6-luna", false, &["low", "medium"]),
    ];

    let config = ComputeConfig::defaults(&catalog).unwrap();
    assert_eq!(config.lane(ComputeLane::Sense).model, "gpt-5.6-luna");
    assert_eq!(config.lane(ComputeLane::Sense).effort, "low");
    assert_eq!(config.lane(ComputeLane::Observe).model, "gpt-5.6-luna");
    assert_eq!(
        config.lane(ComputeLane::Conversation).model,
        "gpt-5.6-terra"
    );
    assert_eq!(config.lane(ComputeLane::Investigate).model, "gpt-5.6-sol");
    assert_eq!(config.lane(ComputeLane::Critical).effort, "xhigh");
}

#[test]
fn defaults_never_fall_back_below_luna() {
    let catalog = vec![
        model("gpt-5.4-mini", true, &["low"]),
        model("gpt-5.3-codex-spark", false, &["low"]),
    ];
    assert!(ComputeConfig::defaults(&catalog).is_err());
}

#[tokio::test]
async fn legacy_mini_is_migrated_without_resetting_other_lanes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compute.toml");
    let catalog = vec![
        model("gpt-5.6-luna", true, &["low", "medium", "xhigh"]),
        model("gpt-5.6-sol", false, &["low", "high", "xhigh"]),
    ];
    let mut config = ComputeConfig::defaults(&catalog).unwrap();
    config.lanes.sense.model = "gpt-5.4-mini".into();
    config.lanes.observe.effort = "xhigh".into();
    config.show_model = false;
    tokio::fs::write(&path, toml::to_string(&config).unwrap())
        .await
        .unwrap();
    let (_, receiver) = tokio::sync::watch::channel(catalog);
    let store = ComputeStore::open(path.clone(), receiver).await.unwrap();
    let migrated = store.snapshot().await;
    assert_eq!(migrated.lanes.sense.model, "gpt-5.6-luna");
    assert_eq!(migrated.lanes.sense.effort, "low");
    assert_eq!(migrated.lanes.observe.effort, "xhigh");
    assert!(!migrated.show_model);
    let persisted: ComputeConfig =
        toml::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
    assert_eq!(persisted.lanes.sense.model, "gpt-5.6-luna");
    assert_eq!(persisted.lanes.critical.model, config.lanes.critical.model);
}

#[tokio::test]
async fn settings_and_validation_follow_the_reconnected_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let luna = model("gpt-5.6-luna", true, &["low", "medium", "high", "xhigh"]);
    let sol = model("gpt-5.6-sol", false, &["low", "high", "xhigh"]);
    let mini = model("gpt-5.4-mini", false, &["low"]);
    let (sender, receiver) =
        tokio::sync::watch::channel(vec![luna.clone(), sol.clone(), mini.clone()]);
    let store = ComputeStore::open(dir.path().join("compute.toml"), receiver)
        .await
        .unwrap();
    assert!(
        store
            .catalog()
            .iter()
            .all(|model| model.model != mini.model)
    );
    let original = store.snapshot().await;
    let mut invalid = original.clone();
    invalid.lanes.sense.model = mini.model;
    assert!(store.update(invalid).await.is_err());
    sender.send_replace(vec![luna]);
    assert_eq!(store.catalog().len(), 1);
    // Sol was valid at startup, but must not remain selectable after reconnect.
    assert!(store.update(original.clone()).await.is_err());
    assert_eq!(store.snapshot().await.lanes.critical.model, sol.model);
}
