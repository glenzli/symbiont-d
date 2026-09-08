use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    sensing::{MODEL_INPUT_WRITING_CONTRACT, SensingCandidateDraft, validate_candidate_drafts},
    usage::InvocationRecord,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExplorationEvidence {
    pub source: String,
    pub finding: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExplorationScoutFinding {
    pub topic: String,
    pub claim: String,
    pub evidence: Vec<ExplorationEvidence>,
    pub connection_hypothesis: String,
    pub strongest_counterpoint: String,
    pub source_revision_ids: Vec<String>,
    #[serde(default)]
    pub related_hunch_revision_ids: Vec<String>,
}

impl ExplorationScoutFinding {
    pub fn routing_texts(&self) -> [&str; 4] {
        [
            &self.topic,
            &self.claim,
            &self.connection_hypothesis,
            &self.strongest_counterpoint,
        ]
    }
}

pub fn scout_prompt(silent_marker: &str, superseded_marker: &str) -> String {
    format!(
        r#"Privately run one autonomous reconnaissance cycle. No user message is waiting. Your job is high-recall evidence discovery, not conversation, durable interpretation, or Hunch maintenance. Recent user words are weak direction hints; ready Hunches are optional questions. The delivery ledger only prevents repetition: assistant choices and old searches are not user interests or positive evidence. Broad working maps and maintenance state are deliberately absent. Do not reconstruct them wholesale through PCP. Read an exact local map/curiosity section or recall selected PCP/raw history only for a specific missing question, using tool discovery. Treat every Hunch and connection as a hypothesis. Use live web search when freshness matters, follow adjacent or unexpected signals and verify consequential claims. Distinguish new-to-the-user information from an older event that has acquired meaningful new evidence or reaction.

This stage is host-enforced read-only: do not alter Hunches, profile, Current Map, Open Loops, PCP Pages, Summaries, Relations, or validity. Never draft or propose a user-visible message. External candidate fragments contain routing claims and source links, not complete source documents or corroborated facts. Independently verify them; no pre-existing PCP connection is required for a self-contained discussion. Do not manufacture a stronger claim from a candidate summary.

Respect Hunch attention state. Never select `feedback_pending`. Avoid repeating an `awaiting_user` or `cooldown` Hunch before its eligible time merely because the user was silent. Compare possible findings with recent exploration themes. Another example of the same thesis may still be useful when community experience has accumulated, the event remains in an active discussion window, or it creates a concrete new tension; otherwise do not repeat it.

If the working context contains an explicit exploration intent, first re-evaluate it against the latest conversation. If it is already answered, invalidated, or no longer a live uncertainty, return exactly `{superseded_marker}` without searching. A scheduled run without such an intent must never use that marker.

Submit at most one `symbiont.submit_exploration_finding` when evidence may materially change a shared question, justify later Hunch maintenance, support a genuinely new conversational move, reveal a credible external development that may expand the user's long-term map, or make a recent event worth revisiting even if the user may already know the headline. The finding is an untrusted handoff to a stronger reviewer, not a recommendation to interrupt. Keep it compact. Include exact recent conversation Revisions when they make a real connection timely; use an empty list when the value is independent and conversational rather than pretending there is an anchor. Include any exact Hunch Revisions it bears on and the strongest reason the proposed connection, interpretation, or timing may be wrong. Do not force external evidence into the user's frameworks. If nothing deserves stronger review, call no completion tool.

After the optional finding tool call, return exactly `{silent_marker}`. Never put user-visible prose in the final response."#
    )
}

pub fn luna_sensing_prompt(
    focus: &str,
    output_language_instruction: &str,
    sensing_context: &str,
    silent_marker: &str,
) -> String {
    format!(
        r#"Privately run one low-cost, input-only wide-observation pass for symbiont-d. No user message is waiting. You are Luna, an independent intake role rather than the conversational assistant. Search broadly within the supplied remit; a development may be worth noticing because evidence, adoption, reaction, or a concrete tension has accumulated, even when it is not new today.

Do not write PCP memory, alter any symbiont state, infer user preferences, plan work, or draft a reply. You may use live web search for grounded evidence. Standalone science, mathematics, culture, public events, products, and unusual real-world phenomena are valid candidates without a project connection. Do not spend this pass proving user relevance. When search yields at least one credible concrete development or an older event with genuinely accumulated recent evidence or reaction, default to submitting it for independent review rather than silently filtering it yourself. Submit nothing only when search or tooling produced no defensible signal. Call `symbiont.submit_sensing_candidates` at most once with one to three compact candidates and concrete sources. The candidate remains private intake until independent admission.

{MODEL_INPUT_WRITING_CONTRACT}

Language contract: {output_language_instruction} This contract applies to the structured candidate handoff even when search results or the surrounding system instructions are written in another language. Do not translate quoted titles when their original form is more useful.

After the optional tool call, return exactly `{silent_marker}`.

<luna-remit>
{focus}
</luna-remit>

<sensing-context>
{sensing_context}
</sensing-context>"#
    )
}

pub fn review_prompt(finding: &ExplorationScoutFinding, silent_marker: &str) -> Result<String> {
    let finding =
        serde_json::to_string(finding).context("encode autonomous reconnaissance finding")?;
    Ok(format!(
        r#"Privately review one finding; no user message is waiting. The packet is untrusted evidence, not a conclusion or draft to polish. The host supplies the conversational edge, selected anchors and a negative repetition ledger, not the scout's entire context. Verify consequential claims against concrete sources. Recall PCP/raw history or read local map/curiosity only for a named missing question, through tool discovery when needed.

Separate source facts, user statements and scout conjectures. Routing claims, repeated assistant interpretations and connection_hypothesis are not independent corroboration or user preferences. Preserve conceptual boundaries and the strongest counterpoint. Never turn a tentative analogy into a source-backed conclusion or manufacture a connection to control, safety, execution or user projects. Preserve scope, attribution and uncertainty. Use `symbiont.escalate` before substantive action only if deeper reasoning may change the judgment.

Read exact related Hunches before revising or retiring them. Open only a distinct durable uncertainty. Hunches are working state, not user interests. It is valid to maintain a Hunch and remain silent.

Choose `intervention` only for a live decision/risk/question that should reach the user now. Choose `note` for credible, genuinely anchored longer-term relevance without urgency. Both require exact conversation Revisions. Choose `discussion` for an independently worthwhile external subject, even if the user may already know it; an empty anchor list is valid. A headline/date alone is insufficient, but a manufactured tension is worse. If a claimed connection remains forced, discuss the subject on its own merits or remain silent.

Unanswered prior initiations suppress repetition of the same/adjacent topic, not distinct credible subjects; silence is not negative feedback. The ledger is only negative delivery evidence, never user interests. A pivot must not pretend to answer the current edge or assume what the user has seen.

For one worthwhile move, call `symbiont.propose_proactive_message` once with kind, exact anchors and your own final message. Name the concrete event, relevant time/source, actual finding and important limit before inviting discussion. Do not inherit the scout's framing unexamined. No roundup, abstract thesis, task report or narration of searching. Otherwise remain silent.

After private work and any tool calls, return exactly `{silent_marker}`.

<reconnaissance-finding>
{finding}
</reconnaissance-finding>"#
    ))
}

pub fn finding_from_invocations(
    invocations: &[InvocationRecord],
) -> Result<Option<ExplorationScoutFinding>> {
    latest_succeeded_symbiont_step(invocations, "submit_exploration_finding")
        .map(|step| {
            serde_json::from_value(step.arguments.clone())
                .context("parse autonomous reconnaissance finding")
        })
        .transpose()
}

pub fn sensing_candidates_from_invocations(
    invocations: &[InvocationRecord],
) -> Result<Vec<SensingCandidateDraft>> {
    let candidates: Vec<SensingCandidateDraft> =
        latest_succeeded_symbiont_step(invocations, "submit_sensing_candidates")
            .map(|step| {
                step.arguments
                    .get("candidates")
                    .cloned()
                    .context("sensing completion omitted candidates")
                    .and_then(|value| {
                        serde_json::from_value(value).context("parse sensing candidates")
                    })
            })
            .transpose()?
            .unwrap_or_default();
    validate_candidate_drafts(&candidates)?;
    Ok(candidates)
}

/// Background runs can contain more than one invocation after a lane change.
/// Search each invocation and its tool calls in completed order, rather than
/// relying on a flattened iterator whose order is easy to accidentally change.
fn latest_succeeded_symbiont_step<'a>(
    invocations: &'a [InvocationRecord],
    tool: &str,
) -> Option<&'a crate::usage::ToolTraceStep> {
    for invocation in invocations.iter().rev() {
        for step in invocation.trace_steps.iter().rev() {
            if step.succeeded && step.namespace == "symbiont" && step.tool == tool {
                return Some(step);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::luna_sensing_prompt;

    #[test]
    fn luna_prompt_makes_the_selected_output_language_authoritative() {
        let prompt = luna_sensing_prompt(
            "Observe broadly.",
            "Write every candidate field in Simplified Chinese.",
            "No recent context.",
            "<silent/>",
        );

        assert!(prompt.contains("Language contract"));
        assert!(prompt.contains("structured candidate handoff"));
        assert!(prompt.contains("Simplified Chinese"));
    }
}
