use serde_json::json;

use super::{ComputeConfig, ComputeLane, ModelInfo};

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
