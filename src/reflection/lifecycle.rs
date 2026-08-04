use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

use super::{
    ConversationEpisode, EpisodeState, HypothesisHorizon, HypothesisInput, ProjectionHealth,
    WorkingHypothesis,
};

const TOPIC_REVIEW_AFTER_DAYS: i64 = 30;
const LIFECYCLE_REVIEW_COOLDOWN_HOURS: i64 = 24;

pub(super) fn validate_hypothesis_review_window(
    input: &HypothesisInput,
    now: DateTime<Utc>,
) -> Result<()> {
    let revisit_after = input
        .revisit_after
        .as_deref()
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .context("hypothesis revisit_after must be RFC 3339")?
        .map(|value| value.with_timezone(&Utc));
    if !input.status.is_active() {
        return Ok(());
    }
    let revisit_after = revisit_after.context(
        "active hypotheses require revisit_after; otherwise mark the projection stale, superseded, or contradicted",
    )?;
    if revisit_after <= now + Duration::minutes(1) {
        anyhow::bail!("active hypothesis revisit_after must be in the future");
    }
    let maximum = match input.horizon {
        HypothesisHorizon::Momentary => Duration::days(7),
        HypothesisHorizon::Current => Duration::days(45),
        HypothesisHorizon::StableCandidate => Duration::days(180),
    };
    if revisit_after > now + maximum {
        anyhow::bail!(
            "hypothesis revisit_after exceeds the {} day maximum for horizon {}",
            maximum.num_days(),
            input.horizon.as_str()
        );
    }
    Ok(())
}

pub(super) fn projection_health(
    episodes: &[ConversationEpisode],
    hypotheses: &[WorkingHypothesis],
    last_lifecycle_review_at: Option<String>,
    now: DateTime<Utc>,
) -> ProjectionHealth {
    let topic_cutoff = now - Duration::days(TOPIC_REVIEW_AFTER_DAYS);
    let active_episodes = episodes
        .iter()
        .filter(|episode| matches!(episode.state, EpisodeState::Forming | EpisodeState::Active))
        .collect::<Vec<_>>();
    let topics_due_for_review = active_episodes
        .iter()
        .filter(|episode| {
            DateTime::parse_from_rfc3339(&episode.last_activity_at)
                .map(|at| at.with_timezone(&Utc) <= topic_cutoff)
                .unwrap_or(true)
        })
        .count();
    let active_hypotheses = hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.status.is_active())
        .collect::<Vec<_>>();
    let hypotheses_missing_revisit = active_hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.revisit_after.is_none())
        .count();
    let hypotheses_due_for_review = active_hypotheses
        .iter()
        .filter_map(|hypothesis| hypothesis.revisit_after.as_deref())
        .filter(|revisit_after| {
            DateTime::parse_from_rfc3339(revisit_after)
                .map(|at| at.with_timezone(&Utc) <= now)
                .unwrap_or(true)
        })
        .count();
    ProjectionHealth {
        active_episode_count: active_episodes.len(),
        active_hypothesis_count: active_hypotheses.len(),
        topics_due_for_review,
        hypotheses_due_for_review,
        hypotheses_missing_revisit,
        last_lifecycle_review_at,
    }
}

pub(super) fn review_due(health: &ProjectionHealth, now: DateTime<Utc>) -> bool {
    if !health.requires_review() {
        return false;
    }
    let Some(last_review) = health.last_lifecycle_review_at.as_deref() else {
        return true;
    };
    let Ok(last_review) = DateTime::parse_from_rfc3339(last_review) else {
        return true;
    };
    now - last_review.with_timezone(&Utc) >= Duration::hours(LIFECYCLE_REVIEW_COOLDOWN_HOURS)
}
