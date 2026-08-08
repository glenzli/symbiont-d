use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tracing::warn;

use crate::{
    ambient_api::{AmbientSenseOutcome, AmbientTopologyStore, LunaInputConfig},
    codex::{CodexClient, RuntimeEvent},
    compute::ComputeConfig,
    profile::ProfileSnapshot,
    sensing::InputRoleSnapshot,
};

/// Owns the Codex-backed execution lifecycle for the built-in Luna intake
/// role. The external-provider topology continues to own configuration and
/// persisted scheduling state.
#[derive(Clone)]
pub struct LunaInput {
    topology: Arc<AmbientTopologyStore>,
}

impl LunaInput {
    pub fn new(topology: Arc<AmbientTopologyStore>) -> Self {
        Self { topology }
    }

    pub async fn sense_selected(
        &self,
        config: LunaInputConfig,
        client: &mut CodexClient,
        compute: &ComputeConfig,
        profile: &ProfileSnapshot,
        sensing_context: &str,
        input_events: watch::Receiver<u64>,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Result<AmbientSenseOutcome> {
        self.topology.mark_luna_started().await?;
        let result = client
            .sense_luna(
                &config.focus,
                sensing_context,
                compute,
                profile,
                input_events,
                events,
            )
            .await;
        match result {
            Ok(outcome) if outcome.interrupted => Ok(AmbientSenseOutcome {
                interrupted: true,
                ..empty_outcome()
            }),
            Ok(outcome) => {
                self.topology.mark_luna_succeeded().await?;
                Ok(AmbientSenseOutcome {
                    invocation: outcome.invocations.first().cloned(),
                    candidates: outcome.candidates,
                    actor: Some(actor(compute)),
                    interrupted: false,
                    channel_failure: None,
                })
            }
            Err(error) => {
                let message = error.to_string();
                self.topology.mark_luna_failed(&message).await?;
                warn!(%error, "built-in Luna input failed without fallback");
                Ok(AmbientSenseOutcome {
                    channel_failure: Some(message),
                    ..empty_outcome()
                })
            }
        }
    }
}

fn actor(compute: &ComputeConfig) -> InputRoleSnapshot {
    InputRoleSnapshot::ambient(
        "luna",
        "Luna · 广域输入",
        &compute.lane(crate::compute::ComputeLane::Sense).model,
        "codex",
    )
}

fn empty_outcome() -> AmbientSenseOutcome {
    AmbientSenseOutcome {
        candidates: Vec::new(),
        invocation: None,
        actor: None,
        interrupted: false,
        channel_failure: None,
    }
}
