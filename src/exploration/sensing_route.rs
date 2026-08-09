//! Pure routing policy between transient intake review and the two delivery
//! paths: attributed external input or exceptional deep Symbiont work.

use std::collections::HashMap;

use crate::{
    codex::{SensingReviewDecision, SensingReviewDisposition},
    sensing::SensingCandidate,
};

#[derive(Clone, Debug)]
pub(super) struct RoutedInput {
    pub candidate: SensingCandidate,
    pub content: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SensingRoutePlan {
    pub inputs: Vec<RoutedInput>,
    pub deep_candidates: Vec<SensingCandidate>,
    pub terminal_ids: Vec<String>,
    pub discarded_count: usize,
}

pub(super) fn plan_sensing_routes(
    candidates: &[SensingCandidate],
    decisions: Vec<SensingReviewDecision>,
) -> SensingRoutePlan {
    let candidates_by_id = candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<HashMap<_, _>>();
    let mut plan = SensingRoutePlan::default();

    for decision in decisions {
        let Some(candidate) = candidates_by_id.get(decision.candidate_id.as_str()) else {
            continue;
        };
        match decision.disposition {
            SensingReviewDisposition::Input => {
                plan.terminal_ids.push(candidate.id.clone());
                let content = decision
                    .input_text
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| candidate.proposed_input.clone());
                plan.inputs.push(RoutedInput {
                    candidate: (*candidate).clone(),
                    content,
                    reason: decision.reason,
                });
            }
            SensingReviewDisposition::Deep => {
                plan.deep_candidates.push((*candidate).clone());
            }
            SensingReviewDisposition::Discard => {
                plan.terminal_ids.push(candidate.id.clone());
                plan.discarded_count += 1;
            }
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensing::{InputRoleSnapshot, SensingSource, SensingSourceClass};

    fn candidate(id: &str, proposed_input: &str) -> SensingCandidate {
        SensingCandidate {
            id: id.to_owned(),
            title: format!("Candidate {id}"),
            summary: "Summary".to_owned(),
            proposed_input: proposed_input.to_owned(),
            event_at: None,
            source_class: SensingSourceClass::OpenDiscovery,
            possible_connection: None,
            sources: vec![SensingSource {
                url: "https://example.test/source".to_owned(),
                detail: "Source".to_owned(),
            }],
            actor: InputRoleSnapshot::mailbox("Research Inbox"),
            observed_at: "2026-08-10T00:00:00.000Z".to_owned(),
            expires_at: "2026-08-11T00:00:00.000Z".to_owned(),
            fingerprint: format!("fingerprint-{id}"),
        }
    }

    #[test]
    fn ordinary_inputs_do_not_enter_the_deep_path() {
        let candidates = vec![candidate("one", "Original external wording")];
        let plan = plan_sensing_routes(
            &candidates,
            vec![SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Input,
                reason: "Interesting external input".to_owned(),
                input_text: None,
            }],
        );

        assert_eq!(plan.inputs.len(), 1);
        assert_eq!(plan.inputs[0].content, "Original external wording");
        assert!(plan.deep_candidates.is_empty());
    }

    #[test]
    fn qualified_input_replaces_overconfident_source_wording_without_changing_actor() {
        let candidates = vec![candidate("one", "This definitely happened")];
        let plan = plan_sensing_routes(
            &candidates,
            vec![SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Input,
                reason: "Interesting but not independently verified".to_owned(),
                input_text: Some("The configured research digest reports this development; the linked claim has not been independently checked here.".to_owned()),
            }],
        );

        assert_eq!(plan.inputs[0].candidate.actor.id, "mail_inbox");
        assert!(plan.inputs[0].content.contains("digest reports"));
    }

    #[test]
    fn only_explicit_high_value_routes_reach_deep_symbiont_work() {
        let candidates = vec![candidate("one", "A consequential signal")];
        let plan = plan_sensing_routes(
            &candidates,
            vec![SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Deep,
                reason: "Changes a durable shared question".to_owned(),
                input_text: None,
            }],
        );

        assert!(plan.inputs.is_empty());
        assert_eq!(plan.deep_candidates.len(), 1);
    }
}
