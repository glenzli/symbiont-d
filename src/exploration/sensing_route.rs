//! Pure routing policy between transient intake review and the two delivery
//! paths: attributed external input or exceptional deep Symbiont work.

use std::collections::HashMap;

use serde_json::json;

use crate::{
    inference::{SensingReviewDecision, SensingReviewDisposition},
    sensing::{InputRoleSnapshot, SensingCandidate, SensingCandidateDraft, SensingPresentation},
    usage::InvocationRecord,
};

pub(super) fn link_sensing_invocations(
    invocations: &mut [InvocationRecord],
    existing_root: Option<&str>,
) -> Option<String> {
    let root = existing_root
        .map(str::to_owned)
        .or_else(|| invocations.first().map(|invocation| invocation.id.clone()))?;
    for (index, invocation) in invocations.iter_mut().enumerate() {
        if existing_root.is_some() || index > 0 {
            invocation.parent_id = Some(root.clone());
        }
    }
    Some(root)
}

pub(super) fn annotate_sensing_delivery(
    invocations: &mut [InvocationRecord],
    published_input_count: usize,
    suppressed_input_count: usize,
    deferred_candidate_count: usize,
) {
    for invocation in invocations.iter_mut().rev() {
        if let Some(step) = invocation.trace_steps.iter_mut().rev().find(|step| {
            step.succeeded
                && step.namespace == "symbiont"
                && step.tool == "review_sensing_candidates"
        }) {
            step.result = json!({
                "accepted": true,
                "hostRouting": {
                    "publishedInputCount": published_input_count,
                    "suppressedInputCount": suppressed_input_count,
                    "deferredCandidateCount": deferred_candidate_count,
                }
            });
            break;
        }
    }
}

pub(super) fn prioritize_candidate_batches(
    mailbox_candidates: Vec<SensingCandidateDraft>,
    mailbox_actor: Option<InputRoleSnapshot>,
    ambient_batches: Vec<(Vec<SensingCandidateDraft>, InputRoleSnapshot)>,
) -> Vec<(Vec<SensingCandidateDraft>, InputRoleSnapshot)> {
    let mut batches = Vec::new();
    if let Some(actor) = mailbox_actor
        && !mailbox_candidates.is_empty()
    {
        batches.push((mailbox_candidates, actor));
    }
    batches.extend(
        ambient_batches
            .into_iter()
            .filter(|(candidates, _)| !candidates.is_empty()),
    );
    batches
}

#[derive(Clone, Debug)]
pub(super) struct RoutedInput {
    pub candidate: SensingCandidate,
    pub content: String,
    pub presentation: SensingPresentation,
    pub qualification_note: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SensingRoutePlan {
    pub inputs: Vec<RoutedInput>,
    pub deep_candidates: Vec<SensingCandidate>,
    pub terminal_ids: Vec<String>,
    pub deferred_ids: Vec<String>,
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

    let mut decided_ids = std::collections::HashSet::new();
    for decision in decisions {
        let Some(candidate) = candidates_by_id.get(decision.candidate_id.as_str()) else {
            continue;
        };
        if !decided_ids.insert(candidate.id.as_str()) {
            continue;
        }
        match decision.disposition {
            SensingReviewDisposition::Input => {
                plan.terminal_ids.push(candidate.id.clone());
                let presentation = decision.presentation.unwrap_or_default();
                let content = match presentation {
                    SensingPresentation::Original => candidate.received_text.clone(),
                    SensingPresentation::Condensed => decision
                        .display_text
                        .filter(|text| !text.trim().is_empty())
                        .unwrap_or_else(|| candidate.proposed_input.clone()),
                };
                plan.inputs.push(RoutedInput {
                    candidate: (*candidate).clone(),
                    content,
                    presentation,
                    qualification_note: decision.qualification_note,
                    reason: decision.reason,
                });
            }
            SensingReviewDisposition::Deep => {
                plan.deep_candidates.push((*candidate).clone());
            }
            SensingReviewDisposition::Discard => {
                plan.terminal_ids.push(candidate.id.clone());
            }
        }
    }

    plan.deferred_ids.extend(
        candidates
            .iter()
            .filter(|candidate| !decided_ids.contains(candidate.id.as_str()))
            .map(|candidate| candidate.id.clone()),
    );

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensing::{InputRoleSnapshot, SensingSource, SensingSourceClass};
    use crate::usage::ToolTraceStep;

    fn candidate(id: &str, proposed_input: &str) -> SensingCandidate {
        SensingCandidate {
            id: id.to_owned(),
            title: format!("Candidate {id}"),
            summary: "Summary".to_owned(),
            proposed_input: proposed_input.to_owned(),
            received_text: proposed_input.to_owned(),
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

    fn draft(title: &str) -> SensingCandidateDraft {
        SensingCandidateDraft {
            title: title.to_owned(),
            summary: "Summary".to_owned(),
            proposed_input: "Input".to_owned(),
            received_text: None,
            event_at: None,
            source_class: SensingSourceClass::OpenDiscovery,
            possible_connection: None,
            sources: vec![SensingSource {
                url: format!("https://example.test/{title}"),
                detail: "Source".to_owned(),
            }],
        }
    }

    #[test]
    fn mailbox_priority_does_not_discard_ambient_batches() {
        let batches = prioritize_candidate_batches(
            vec![draft("mail")],
            Some(InputRoleSnapshot::mailbox("Research Inbox")),
            vec![(
                vec![draft("ambient")],
                InputRoleSnapshot::ambient("luna", "Luna", "test", "codex"),
            )],
        );

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].0[0].title, "mail");
        assert_eq!(batches[1].0[0].title, "ambient");
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
                presentation: Some(SensingPresentation::Original),
                display_text: None,
                qualification_note: None,
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
                presentation: Some(SensingPresentation::Condensed),
                display_text: Some("The configured research digest reports this development; the linked claim has not been independently checked here.".to_owned()),
                qualification_note: Some("The linked claim was not independently checked here.".to_owned()),
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
                presentation: None,
                display_text: None,
                qualification_note: None,
            }],
        );

        assert!(plan.inputs.is_empty());
        assert_eq!(plan.deep_candidates.len(), 1);
    }

    #[test]
    fn candidates_omitted_by_the_reviewer_are_deferred_instead_of_lost() {
        let candidates = vec![candidate("one", "First"), candidate("two", "Second")];
        let plan = plan_sensing_routes(
            &candidates,
            vec![SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Discard,
                reason: "Noise".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
            }],
        );

        assert_eq!(plan.terminal_ids, vec!["one"]);
        assert_eq!(plan.deferred_ids, vec!["two"]);
    }

    #[test]
    fn sensing_trace_links_channels_and_records_actual_host_delivery() {
        let mut invocations = vec![
            sensing_invocation("root", None, false),
            sensing_invocation("second", None, false),
        ];
        let root = link_sensing_invocations(&mut invocations, None).unwrap();
        assert_eq!(root, "root");
        assert_eq!(invocations[1].parent_id.as_deref(), Some("root"));

        let mut review = vec![sensing_invocation("review", None, true)];
        link_sensing_invocations(&mut review, Some(&root));
        annotate_sensing_delivery(&mut review, 2, 1, 1);

        assert_eq!(review[0].parent_id.as_deref(), Some("root"));
        assert_eq!(
            review[0].trace_steps[0]
                .result
                .pointer("/hostRouting/publishedInputCount")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
    }

    fn sensing_invocation(id: &str, parent_id: Option<&str>, review: bool) -> InvocationRecord {
        InvocationRecord {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            thread_id: format!("thread-{id}"),
            turn_id: format!("turn-{id}"),
            origin: if review {
                "ambient_review"
            } else {
                "ambient_sense"
            }
            .to_owned(),
            lane: "sense".to_owned(),
            requested_model: "test".to_owned(),
            effective_model: "test".to_owned(),
            model_display_name: "Test".to_owned(),
            effort: "low".to_owned(),
            service_tier: None,
            started_at: "2026-08-10T00:00:00.000Z".to_owned(),
            completed_at: "2026-08-10T00:00:01.000Z".to_owned(),
            duration_ms: 1_000,
            status: "completed".to_owned(),
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            tool_calls: vec![],
            produced_message: false,
            trace_steps: review
                .then(|| ToolTraceStep {
                    sequence: 0,
                    namespace: "symbiont".to_owned(),
                    tool: "review_sensing_candidates".to_owned(),
                    started_at: "2026-08-10T00:00:00.000Z".to_owned(),
                    completed_at: "2026-08-10T00:00:01.000Z".to_owned(),
                    duration_ms: 1_000,
                    succeeded: true,
                    arguments: serde_json::json!({"decisions": []}),
                    result: serde_json::json!({"accepted": true}),
                })
                .into_iter()
                .collect(),
            context_snapshot: None,
            trace_events: vec![],
        }
    }
}
