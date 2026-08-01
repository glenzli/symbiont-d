use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    sync::{Mutex, mpsc},
    time::sleep,
};
use tracing::{debug, warn};

use crate::{
    autonomy::AutonomyStore,
    codex::CodexClient,
    compute::ComputeStore,
    continuity::ContinuityHost,
    conversation::ConversationCoordinator,
    exploration::today_started_at,
    profile::{ProfileStore, SetupStatus},
    usage::UsageStore,
};

const INITIAL_DELAY: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_secs(60);
const IDLE_DELAY: Duration = Duration::from_secs(300);
// This only excludes obviously short events. The model decides whether a
// candidate is semantically dense enough to deserve a Summary projection.
const MINIMUM_SUMMARY_CHARS: usize = 1_200;

pub fn start(
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    usage: Arc<UsageStore>,
    conversation: ConversationCoordinator,
) {
    tokio::spawn(run(
        autonomy,
        profile,
        codex,
        compute,
        continuity,
        usage,
        conversation,
    ));
}

async fn run(
    autonomy: Arc<AutonomyStore>,
    profile: Arc<ProfileStore>,
    codex: Arc<Mutex<CodexClient>>,
    compute: Arc<ComputeStore>,
    continuity: Arc<ContinuityHost>,
    usage: Arc<UsageStore>,
    conversation: ConversationCoordinator,
) {
    sleep(INITIAL_DELAY).await;
    loop {
        let delay = match maintain_one(
            &autonomy,
            &profile,
            &codex,
            &compute,
            &continuity,
            &usage,
            &conversation,
        )
        .await
        {
            Ok(MaintenanceState::Maintained) => RETRY_DELAY,
            Ok(MaintenanceState::Busy) => RETRY_DELAY,
            Ok(MaintenanceState::Idle) => IDLE_DELAY,
            Err(error) => {
                warn!(%error, "PCP Summary maintenance failed");
                RETRY_DELAY
            }
        };
        sleep(delay).await;
    }
}

enum MaintenanceState {
    Maintained,
    Busy,
    Idle,
}

async fn maintain_one(
    autonomy: &AutonomyStore,
    profile: &ProfileStore,
    codex: &Mutex<CodexClient>,
    compute: &ComputeStore,
    continuity: &ContinuityHost,
    usage: &UsageStore,
    conversation: &ConversationCoordinator,
) -> Result<MaintenanceState> {
    let autonomy = autonomy.snapshot().await;
    let profile = profile.snapshot().await;
    if !autonomy.enabled || profile.status != SetupStatus::Ready {
        return Ok(MaintenanceState::Idle);
    }
    let headline = usage.headline(&today_started_at()).await?;
    if autonomy.daily_token_limit > 0
        && headline.autonomous_tokens_today >= autonomy.daily_token_limit
    {
        return Ok(MaintenanceState::Idle);
    }
    let Some(target_revision_id) = continuity
        .next_summary_candidate(MINIMUM_SUMMARY_CHARS)
        .await?
    else {
        return Ok(MaintenanceState::Idle);
    };
    let Ok(mut client) = codex.try_lock() else {
        return Ok(MaintenanceState::Busy);
    };

    let compute = compute.snapshot().await;
    let continuity_context = continuity.context_seed(None).await;
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let event_drain = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });
    let outcome = client
        .maintain_summary(
            &target_revision_id,
            &compute,
            &profile,
            &continuity_context,
            conversation.subscribe_input(),
            events_tx,
        )
        .await;
    drop(client);
    event_drain
        .await
        .context("join PCP Summary maintenance event drain")?;
    let outcome = outcome?;
    usage.record_all(&outcome.invocations).await?;
    if outcome.interrupted {
        return Ok(MaintenanceState::Busy);
    }
    continuity
        .mark_summary_assessed(
            target_revision_id.clone(),
            if outcome.summarized {
                "summarized"
            } else {
                "skipped"
            },
            outcome.model,
        )
        .await?;
    debug!(
        target_revision_id,
        summarized = outcome.summarized,
        "PCP Summary candidate assessed"
    );
    Ok(MaintenanceState::Maintained)
}
