use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use tokio::sync::RwLock;

const MAX_ACTIVE_HUMAN_TURNS: u8 = 6;
const MAX_CONSECUTIVE_SILENT_TURNS: u8 = 2;
const MAX_CONSECUTIVE_FAILED_TURNS: u8 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CouncilScope {
    topic_id: Option<String>,
}

impl CouncilScope {
    pub(crate) fn from_topic(topic_id: Option<&str>) -> Self {
        Self {
            topic_id: topic_id
                .map(str::trim)
                .filter(|topic_id| !topic_id.is_empty())
                .map(str::to_owned),
        }
    }

    pub(crate) fn key(&self) -> String {
        self.topic_id
            .as_ref()
            .map(|id| format!("topic:{id}"))
            .unwrap_or_else(|| "main".to_owned())
    }

    pub(crate) fn topic_id(&self) -> Option<&str> {
        self.topic_id.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParticipationAction {
    Respond,
    Silent,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParticipationContinuation {
    Stay,
    Leave,
}

#[derive(Clone, Debug)]
pub(crate) struct ParticipationOutcome {
    pub(crate) participant_id: String,
    pub(crate) action: ParticipationAction,
    pub(crate) continuation: ParticipationContinuation,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveCouncilParticipant {
    pub participant_id: String,
    pub name: String,
    pub avatar: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCouncilActivationSnapshot {
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<String>,
    pub participants: Vec<ActiveCouncilParticipant>,
}

#[derive(Clone, Debug)]
struct ActiveParticipant {
    remaining_turns: u8,
    consecutive_silent_turns: u8,
    consecutive_failed_turns: u8,
}

impl ActiveParticipant {
    fn renewed() -> Self {
        Self {
            remaining_turns: MAX_ACTIVE_HUMAN_TURNS,
            consecutive_silent_turns: 0,
            consecutive_failed_turns: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct ActivationRegistry {
    scopes: RwLock<BTreeMap<String, BTreeMap<String, ActiveParticipant>>>,
}

impl ActivationRegistry {
    pub(crate) async fn active_ids(&self, scope: &CouncilScope) -> Vec<String> {
        self.scopes
            .read()
            .await
            .get(&scope.key())
            .map(|participants| participants.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) async fn begin_turn(
        &self,
        scope: &CouncilScope,
        valid_active_ids: &[String],
        directly_mentioned_ids: &[String],
    ) -> Vec<String> {
        let valid = valid_active_ids.iter().collect::<BTreeSet<_>>();
        let mut scopes = self.scopes.write().await;
        let scope_key = scope.key();
        let active_ids = {
            let participants = scopes.entry(scope_key.clone()).or_default();
            participants.retain(|id, _| valid.contains(id));
            for id in directly_mentioned_ids {
                participants.insert(id.clone(), ActiveParticipant::renewed());
            }
            participants.keys().cloned().collect::<Vec<_>>()
        };
        if active_ids.is_empty() {
            scopes.remove(&scope_key);
        }
        active_ids
    }

    pub(crate) async fn finish_turn(
        &self,
        scope: &CouncilScope,
        outcomes: &[ParticipationOutcome],
    ) -> Vec<String> {
        let mut scopes = self.scopes.write().await;
        let scope_key = scope.key();
        let Some(participants) = scopes.get_mut(&scope_key) else {
            return Vec::new();
        };
        for outcome in outcomes {
            let Some(active) = participants.get_mut(&outcome.participant_id) else {
                continue;
            };
            if outcome.action == ParticipationAction::Interrupted {
                continue;
            }
            if outcome.continuation == ParticipationContinuation::Leave {
                participants.remove(&outcome.participant_id);
                continue;
            }
            active.remaining_turns = active.remaining_turns.saturating_sub(1);
            match outcome.action {
                ParticipationAction::Respond => {
                    active.consecutive_silent_turns = 0;
                    active.consecutive_failed_turns = 0;
                }
                ParticipationAction::Silent => {
                    active.consecutive_silent_turns =
                        active.consecutive_silent_turns.saturating_add(1);
                    active.consecutive_failed_turns = 0;
                }
                ParticipationAction::Failed => {
                    active.consecutive_failed_turns =
                        active.consecutive_failed_turns.saturating_add(1);
                }
                ParticipationAction::Interrupted => {}
            }
        }
        participants.retain(|_, active| {
            active.remaining_turns > 0
                && active.consecutive_silent_turns < MAX_CONSECUTIVE_SILENT_TURNS
                && active.consecutive_failed_turns < MAX_CONSECUTIVE_FAILED_TURNS
        });
        let active_ids = participants.keys().cloned().collect();
        if participants.is_empty() {
            scopes.remove(&scope_key);
        }
        active_ids
    }

    pub(crate) async fn deactivate(&self, scope: &CouncilScope, participant_id: &str) {
        let mut scopes = self.scopes.write().await;
        let scope_key = scope.key();
        let Some(participants) = scopes.get_mut(&scope_key) else {
            return;
        };
        participants.remove(participant_id);
        if participants.is_empty() {
            scopes.remove(&scope_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_mention_renews_a_bounded_topic_lease() {
        let registry = ActivationRegistry::default();
        let scope = CouncilScope::from_topic(Some("topic-one"));
        assert_eq!(
            registry.begin_turn(&scope, &[], &["peer".to_owned()]).await,
            vec!["peer"]
        );

        for _ in 0..MAX_ACTIVE_HUMAN_TURNS - 1 {
            assert_eq!(
                registry
                    .finish_turn(
                        &scope,
                        &[ParticipationOutcome {
                            participant_id: "peer".to_owned(),
                            action: ParticipationAction::Respond,
                            continuation: ParticipationContinuation::Stay,
                        }],
                    )
                    .await,
                vec!["peer"]
            );
        }
        assert!(
            registry
                .finish_turn(
                    &scope,
                    &[ParticipationOutcome {
                        participant_id: "peer".to_owned(),
                        action: ParticipationAction::Respond,
                        continuation: ParticipationContinuation::Stay,
                    }],
                )
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn model_can_leave_and_silence_has_a_hard_stop() {
        let registry = ActivationRegistry::default();
        let scope = CouncilScope::from_topic(None);
        registry
            .begin_turn(&scope, &[], &["leaver".to_owned(), "quiet".to_owned()])
            .await;
        assert_eq!(
            registry
                .finish_turn(
                    &scope,
                    &[
                        ParticipationOutcome {
                            participant_id: "leaver".to_owned(),
                            action: ParticipationAction::Respond,
                            continuation: ParticipationContinuation::Leave,
                        },
                        ParticipationOutcome {
                            participant_id: "quiet".to_owned(),
                            action: ParticipationAction::Silent,
                            continuation: ParticipationContinuation::Stay,
                        },
                    ],
                )
                .await,
            vec!["quiet"]
        );
        assert!(
            registry
                .finish_turn(
                    &scope,
                    &[ParticipationOutcome {
                        participant_id: "quiet".to_owned(),
                        action: ParticipationAction::Silent,
                        continuation: ParticipationContinuation::Stay,
                    }],
                )
                .await
                .is_empty()
        );
    }
}
