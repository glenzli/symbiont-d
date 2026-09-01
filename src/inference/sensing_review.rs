use std::collections::HashSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sensing::{SensingCandidate, SensingPresentation};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SensingReviewDisposition {
    Discard,
    Input,
    Deep,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SensingReviewDecision {
    pub candidate_id: String,
    pub disposition: SensingReviewDisposition,
    pub reason: String,
    #[serde(default)]
    pub presentation: Option<SensingPresentation>,
    #[serde(default)]
    pub display_text: Option<String>,
    #[serde(default)]
    pub qualification_note: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SensingReviewEnvelope {
    pub decisions: Vec<SensingReviewDecision>,
}

pub(super) fn runtime_prompt(candidates: &[SensingCandidate]) -> Result<String> {
    let candidates = serde_json::to_string_pretty(candidates)
        .context("encode ambient sensing review candidates")?;
    Ok(format!(
        r#"Review the following short-lived external-signal candidates for symbiont-d. No user
message is waiting. You are a gate, not a co-author: candidates come from input-only model roles
and their wording must remain attributable to those roles if it enters the input stream.

Duplicate suppression has already run independently. Evaluate every remaining candidate on its own
and choose exactly one route. You cannot browse in this stage, so assess only the supplied packet
and never pretend that you opened or verified its links:

- `discard`: a clear duplicate, spam/noise, unsafe material, internal incoherence, or a claim contradicted by the supplied packet;
- `input`: an attributed, independently interesting external input that should enter the temporary input stream without symbiont-d taking over its voice;
- `deep`: an unusually consequential or generative candidate that merits expensive investigation and a separate decision about whether symbiont-d itself should speak.

`input` is not an autonomous symbiont-d interruption: it is a temporary, attributed input-role
message with a source trail and a reply affordance, not a symbiont-d endorsement or durable claim.
Its admission bar is therefore lower than the bar for an assistant-authored note or intervention.
Weak or secondary sourcing alone is not a reason to discard a plausible, interesting input and is
never by itself a reason to choose `deep`. Keep the received text intact: choose
`presentation: original` and omit `display_text`. Length, multiple papers, formulas, detailed
conditions, or awkward framing are not permission to replace the body with a summary or topic list.
Do not reduce a research digest to "this digest covers four papers" or erase its actual results,
assumptions, mechanisms, numbers, formulas, and links. Deterministic removal of already-delivered
sections is handled separately; you do not own source editing.
If the packet needs a sourcing or confidence caveat, put that short caveat in `qualification_note`
rather than replacing the received message. This is qualification, not verification: do not add
facts or say you checked a link. Any generated `qualification_note` must use the same language as that
candidate's `received_text`; never switch an input role's language during review. The internal
`reason` may be concise English.
Reserve `deep` for value, not for ordinary source cleanup. Do not infer that the user is unaware of
an event.

Standalone science, mathematics, culture, public events, products, and unusual real-world phenomena
may be worthwhile without a current-project connection or immediate decision. Relevance to the
current project is not an admission requirement. A recent or still-developing discussion can be
timely even when the original event was not today, provided the supplied packet names the accumulated
evidence or reaction that makes it live now. Judge atomic candidates separately; weakness in a
neighboring digest item must not poison another candidate. The candidate pool is not memory
and this review must not write PCP, Hunches, profile, or other state.

Return exactly one JSON object and no Markdown or commentary. Its only top-level field is
`decisions`, whose value is an array using the fields `candidate_id`, `disposition`, `reason`, and
optional `presentation`, `display_text`, and `qualification_note`. Return exactly one decision for
every supplied candidate.

<ambient-candidates>
{candidates}
</ambient-candidates>"#
    ))
}

pub(super) struct ValidatedDecisions {
    pub(super) decisions: Vec<SensingReviewDecision>,
    pub(super) rejected_count: usize,
    pub(super) missing_count: usize,
}

/// Keeps valid decisions independently so one malformed ID or missing reason
/// only defers that candidate instead of rolling back the entire batch.
pub(super) fn validated_decisions(
    candidates: &[SensingCandidate],
    decisions: Vec<SensingReviewDecision>,
) -> ValidatedDecisions {
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect::<HashSet<_>>();
    let mut decided = HashSet::new();
    let mut valid = Vec::new();
    let mut rejected_count = 0;
    for mut decision in decisions {
        let valid_decision = candidate_ids.contains(decision.candidate_id.as_str())
            && !decided.contains(decision.candidate_id.as_str())
            && !decision.reason.trim().is_empty();
        if valid_decision {
            // Older models may still return display rewrites. They cannot change
            // the admitted source, nor defer an otherwise valid input.
            decision.presentation = Some(SensingPresentation::Original);
            decision.display_text = None;
            decided.insert(decision.candidate_id.clone());
            valid.push(decision);
        } else {
            rejected_count += 1;
        }
    }
    ValidatedDecisions {
        missing_count: candidates.len().saturating_sub(valid.len()),
        decisions: valid,
        rejected_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensing::{InputRoleSnapshot, SensingSource, SensingSourceClass};

    fn candidate(id: &str) -> SensingCandidate {
        SensingCandidate {
            id: id.to_owned(),
            title: "title".to_owned(),
            summary: "summary".to_owned(),
            proposed_input: "input".to_owned(),
            received_text: "input".to_owned(),
            event_at: None,
            source_document_at: None,
            source_class: SensingSourceClass::OpenDiscovery,
            sources: vec![SensingSource {
                url: "https://example.com".to_owned(),
                detail: "source".to_owned(),
            }],
            possible_connection: None,
            actor: InputRoleSnapshot::ambient("test", "Test", "model", "provider"),
            observed_at: "2026-08-11T00:00:00Z".to_owned(),
            expires_at: "2026-08-12T00:00:00Z".to_owned(),
            fingerprint: format!("fingerprint-{id}"),
        }
    }

    #[test]
    fn keeps_valid_decisions_when_a_neighbor_is_duplicate_or_unknown() {
        let candidates = vec![candidate("one"), candidate("two")];
        let decisions = vec![
            SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Input,
                reason: "useful".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
            },
            SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Discard,
                reason: "duplicate".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
            },
            SensingReviewDecision {
                candidate_id: "missing".to_owned(),
                disposition: SensingReviewDisposition::Discard,
                reason: "unknown".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
            },
        ];
        let validation = validated_decisions(&candidates, decisions);
        assert_eq!(validation.decisions.len(), 1);
        assert_eq!(validation.decisions[0].candidate_id, "one");
        assert_eq!(validation.rejected_count, 2);
        assert_eq!(validation.missing_count, 1);
    }

    #[test]
    fn review_prompt_preserves_each_candidate_language() {
        let prompt = runtime_prompt(&[candidate("one")]).unwrap();
        assert!(prompt.contains("must use the same language as that"));
        assert!(prompt.contains("candidate's `received_text`"));
        assert!(prompt.contains("never switch an input role's language"));
    }

    #[test]
    fn review_prompt_does_not_own_duplicate_suppression() {
        let prompt = runtime_prompt(&[candidate("one")]).unwrap();
        assert!(prompt.contains("Duplicate suppression has already run independently"));
        assert!(!prompt.contains("duplicate_of"));
        assert!(!prompt.contains("recent-external-inputs"));
    }

    #[test]
    fn legacy_condensed_presentation_cannot_replace_or_block_the_source() {
        let candidates = vec![candidate("one"), candidate("two")];
        let decisions = vec![SensingReviewDecision {
            candidate_id: "one".to_owned(),
            disposition: SensingReviewDisposition::Input,
            reason: "Worth showing".to_owned(),
            presentation: Some(SensingPresentation::Condensed),
            display_text: None,
            qualification_note: None,
        }];
        let validation = validated_decisions(&candidates, decisions);
        assert_eq!(validation.decisions.len(), 1);
        assert_eq!(
            validation.decisions[0].presentation,
            Some(SensingPresentation::Original)
        );
        assert!(validation.decisions[0].display_text.is_none());
        assert_eq!(validation.rejected_count, 0);
        assert_eq!(validation.missing_count, 1);
    }

    #[test]
    fn review_prompt_does_not_summarize_away_research_details() {
        let prompt = runtime_prompt(&[candidate("one")]).unwrap();
        assert!(prompt.contains("Keep the received text intact"));
        assert!(prompt.contains("assumptions, mechanisms, numbers, formulas, and links"));
        assert!(!prompt.contains("Choose `presentation: condensed`"));
    }
}
