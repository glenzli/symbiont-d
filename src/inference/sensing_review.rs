use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    sensing::{SensingCandidate, SensingPresentation},
    signals::SignalDeduplicationReference,
};

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
    #[serde(default)]
    pub duplicate_of: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SensingReviewEnvelope {
    pub decisions: Vec<SensingReviewDecision>,
}

pub(super) fn runtime_prompt(
    candidates: &[SensingCandidate],
    recent_signals: &[SignalDeduplicationReference],
) -> Result<String> {
    let candidates = serde_json::to_string_pretty(candidates)
        .context("encode ambient sensing review candidates")?;
    let recent_signals = serde_json::to_string_pretty(recent_signals)
        .context("encode recent external input references")?;
    Ok(format!(
        r#"Review the following short-lived external-signal candidates for symbiont-d. No user
message is waiting. You are a gate, not a co-author: candidates come from input-only model roles
and their wording must remain attributable to those roles if it enters the input stream.

First compare the candidates with one another and with the bounded recent-input references, then
choose exactly one route for every candidate. You cannot browse in this stage, so assess only the
supplied packet and never pretend that you opened or verified its links:

- `discard`: a clear duplicate, spam/noise, unsafe material, internal incoherence, or a claim contradicted by the supplied packet;
- `input`: an attributed, independently interesting external input that should enter the temporary input stream without symbiont-d taking over its voice;
- `deep`: an unusually consequential or generative candidate that merits expensive investigation and a separate decision about whether symbiont-d itself should speak.

`input` is not an autonomous symbiont-d interruption: it is a temporary, attributed input-role
message with a source trail and a reply affordance, not a symbiont-d endorsement or durable claim.
Its admission bar is therefore lower than the bar for an assistant-authored note or intervention.
Weak or secondary sourcing alone is not a reason to discard a plausible, interesting input and is
never by itself a reason to choose `deep`. Preserve the received text by default: choose
`presentation: original` and omit `display_text`. Choose `presentation: condensed` only when the
received text is repetitive, excessively long, poorly structured, or contains substantial low-value
framing. In that case provide a self-contained `display_text` and state the concrete compression
reason in `reason`; never condense merely to make the prose sound more polished. If the packet needs
a sourcing or confidence caveat, put that short caveat in `qualification_note` rather than replacing
the received message. This is qualification, not verification: do not add facts or say you checked a
link. Any generated `display_text` and `qualification_note` must use the same language as that
candidate's `received_text`; never switch an input role's language during review. The internal
`reason` may be concise English.
Reserve `deep` for value, not for ordinary source cleanup. Do not infer that the user is unaware of
an event.

Suppress repeated delivery conservatively. A duplicate must describe the same underlying paper,
release, event, observation, or materially identical claim, not merely the same subject. A new
version, later confirmation, changed result, or meaningful evidence update is not a duplicate. If
two current candidates are duplicates, keep the better-supported representative as `input` or
`deep` and mark only the redundant candidate `discard`, with `duplicate_of` set to that surviving
candidate's `candidate_id`. If a candidate repeats a recent external input, mark it `discard` and
set `duplicate_of` to the reference's `signal_id`. Do not set `duplicate_of` for ordinary spam,
noise, safety, or coherence discards. Never make a duplicate chain or point at another discarded
candidate.

Standalone science, mathematics, culture, public events, products, and unusual real-world phenomena
may be worthwhile without a current-project connection or immediate decision. Relevance to the
current project is not an admission requirement. A recent or still-developing discussion can be
timely even when the original event was not today, provided the supplied packet names the accumulated
evidence or reaction that makes it live now. Judge atomic candidates separately; weakness in a
neighboring digest item must not poison another candidate. `display_text`, when used, must remain in
the external input role rather than symbiont-d's voice. The candidate pool is not memory
and this review must not write PCP, Hunches, profile, or other state.

Return exactly one JSON object and no Markdown or commentary. Its only top-level field is
`decisions`, whose value is an array using the fields `candidate_id`, `disposition`, `reason`, and
optional `presentation`, `display_text`, `qualification_note`, and `duplicate_of`. Return exactly
one decision for every supplied candidate.

<ambient-candidates>
{candidates}
</ambient-candidates>

<recent-external-inputs>
{recent_signals}
</recent-external-inputs>"#
    ))
}

pub(super) fn validate_decisions(
    candidates: &[SensingCandidate],
    recent_signals: &[SignalDeduplicationReference],
    decisions: &[SensingReviewDecision],
) -> Result<()> {
    anyhow::ensure!(
        decisions.len() == candidates.len(),
        "sensing review must decide every candidate exactly once"
    );
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect::<HashSet<_>>();
    let recent_signal_ids = recent_signals
        .iter()
        .map(|signal| signal.signal_id.as_str())
        .collect::<HashSet<_>>();
    let decisions_by_id = decisions
        .iter()
        .map(|decision| (decision.candidate_id.as_str(), decision))
        .collect::<HashMap<_, _>>();
    let mut decided = HashSet::new();
    for decision in decisions {
        anyhow::ensure!(
            candidate_ids.contains(decision.candidate_id.as_str()),
            "sensing review returned an unknown candidate"
        );
        anyhow::ensure!(
            decided.insert(decision.candidate_id.as_str()),
            "sensing review returned a duplicate candidate"
        );
        anyhow::ensure!(
            !decision.reason.trim().is_empty(),
            "sensing review omitted its reason"
        );
        if decision.presentation == Some(SensingPresentation::Condensed) {
            anyhow::ensure!(
                decision
                    .display_text
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()),
                "condensed sensing review omitted display text"
            );
        }
        if let Some(duplicate_of) = decision.duplicate_of.as_deref() {
            anyhow::ensure!(
                decision.disposition == SensingReviewDisposition::Discard,
                "only a discarded sensing candidate may name a duplicate target"
            );
            anyhow::ensure!(
                duplicate_of != decision.candidate_id,
                "sensing candidate cannot duplicate itself"
            );
            anyhow::ensure!(
                candidate_ids.contains(duplicate_of) || recent_signal_ids.contains(duplicate_of),
                "sensing duplicate target is not in the supplied comparison set"
            );
            if let Some(target) = decisions_by_id.get(duplicate_of) {
                anyhow::ensure!(
                    target.disposition != SensingReviewDisposition::Discard
                        && target.duplicate_of.is_none(),
                    "sensing duplicate target must be a surviving current candidate"
                );
            }
        }
    }
    Ok(())
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
    fn rejects_missing_or_duplicate_candidate_decisions() {
        let candidates = vec![candidate("one"), candidate("two")];
        let decisions = vec![
            SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Input,
                reason: "useful".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
                duplicate_of: None,
            },
            SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Discard,
                reason: "duplicate".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
                duplicate_of: None,
            },
        ];
        assert!(validate_decisions(&candidates, &[], &decisions).is_err());
    }

    #[test]
    fn review_prompt_preserves_each_candidate_language() {
        let prompt = runtime_prompt(&[candidate("one")], &[]).unwrap();
        assert!(prompt.contains("must use the same language as that"));
        assert!(prompt.contains("candidate's `received_text`"));
        assert!(prompt.contains("never switch an input role's language"));
    }

    #[test]
    fn review_prompt_compares_recent_inputs_without_equating_topics() {
        let recent = vec![SignalDeduplicationReference {
            signal_id: "signal-existing".to_owned(),
            actor_name: "Another input role".to_owned(),
            title: "Same event in different words".to_owned(),
            excerpt: "A bounded prior description".to_owned(),
            source_urls: vec!["https://example.com/prior".to_owned()],
            event_at: Some("2026-08-10".to_owned()),
            observed_at: "2026-08-10T12:00:00Z".to_owned(),
        }];
        let prompt = runtime_prompt(&[candidate("one")], &recent).unwrap();

        assert!(prompt.contains("signal-existing"));
        assert!(prompt.contains("same underlying paper"));
        assert!(prompt.contains("not merely the same subject"));
        assert!(prompt.contains("meaningful evidence update is not a duplicate"));
    }

    #[test]
    fn duplicate_targets_must_exist_and_survive_the_current_review() {
        let candidates = vec![candidate("one"), candidate("two")];
        let valid = vec![
            SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Input,
                reason: "Best supported representative".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
                duplicate_of: None,
            },
            SensingReviewDecision {
                candidate_id: "two".to_owned(),
                disposition: SensingReviewDisposition::Discard,
                reason: "Same event as the first candidate".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
                duplicate_of: Some("one".to_owned()),
            },
        ];
        assert!(validate_decisions(&candidates, &[], &valid).is_ok());

        let mut invalid = valid;
        invalid[0].disposition = SensingReviewDisposition::Discard;
        assert!(validate_decisions(&candidates, &[], &invalid).is_err());
        invalid[0].disposition = SensingReviewDisposition::Input;
        invalid[1].duplicate_of = Some("missing".to_owned());
        assert!(validate_decisions(&candidates, &[], &invalid).is_err());
    }

    #[test]
    fn a_recent_signal_is_a_valid_duplicate_target() {
        let candidates = vec![candidate("one")];
        let recent = vec![SignalDeduplicationReference {
            signal_id: "signal-existing".to_owned(),
            actor_name: "Earlier role".to_owned(),
            title: "Earlier message".to_owned(),
            excerpt: "The same event".to_owned(),
            source_urls: vec!["https://example.com/earlier".to_owned()],
            event_at: None,
            observed_at: "2026-08-10T12:00:00Z".to_owned(),
        }];
        let decisions = vec![SensingReviewDecision {
            candidate_id: "one".to_owned(),
            disposition: SensingReviewDisposition::Discard,
            reason: "Already shown".to_owned(),
            presentation: None,
            display_text: None,
            qualification_note: None,
            duplicate_of: Some("signal-existing".to_owned()),
        }];

        assert!(validate_decisions(&candidates, &recent, &decisions).is_ok());
    }
}
