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

pub(crate) fn codex_prompt(candidates: &[SensingCandidate], silent_marker: &str) -> Result<String> {
    prompt(
        candidates,
        &format!(
            "Call `symbiont.review_sensing_candidates` exactly once. After the tool call, return exactly\n`{silent_marker}`."
        ),
    )
}

pub(super) fn runtime_prompt(candidates: &[SensingCandidate]) -> Result<String> {
    prompt(
        candidates,
        "Return exactly one JSON object and no Markdown or commentary. Its only top-level field is `decisions`, whose value is an array using the fields `candidate_id`, `disposition`, `reason`, and optional `presentation`, `display_text`, and `qualification_note`. Return exactly one decision for every supplied candidate.",
    )
}

fn prompt(candidates: &[SensingCandidate], completion: &str) -> Result<String> {
    let candidates = serde_json::to_string_pretty(candidates)
        .context("encode ambient sensing review candidates")?;
    Ok(format!(
        r#"Review the following short-lived external-signal candidates for symbiont-d. No user
message is waiting. You are a gate, not a co-author: candidates come from input-only model roles
and their wording must remain attributable to those roles if it enters the input stream.

Evaluate every candidate independently and choose exactly one route. You cannot browse in this
stage, so assess only the supplied packet and never pretend that you opened or verified its links:

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
link.
Reserve `deep` for value, not for ordinary source cleanup. Do not infer that the user is unaware of
an event.

Standalone science, mathematics, culture, public events, products, and unusual real-world phenomena
may be worthwhile without a current-project connection or immediate decision. Relevance to the
current project is not an admission requirement. A recent or still-developing discussion can be
timely even when the original event was not today, provided the supplied packet names the accumulated
evidence or reaction that makes it live now. Judge atomic candidates separately; weakness in a
neighboring digest item must not poison another candidate. `display_text`, when used, must remain in
the external input role rather than symbiont-d's voice. The candidate pool is not memory
and this review must not write PCP, Hunches, profile, or other state.

{completion}

<ambient-candidates>
{candidates}
</ambient-candidates>"#
    ))
}

pub(super) fn validate_decisions(
    candidates: &[SensingCandidate],
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
            },
            SensingReviewDecision {
                candidate_id: "one".to_owned(),
                disposition: SensingReviewDisposition::Discard,
                reason: "duplicate".to_owned(),
                presentation: None,
                display_text: None,
                qualification_note: None,
            },
        ];
        assert!(validate_decisions(&candidates, &decisions).is_err());
    }
}
