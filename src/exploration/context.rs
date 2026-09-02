//! Exploration-only input selection. Positive evidence, weak interest hints and
//! negative delivery evidence have distinct owners and are never blended.
use chrono::{DateTime, Utc};
use serde_json::json;

use super::{ExplorationTrigger, bounded_message_excerpt, conversation_edge};
use crate::{
    context_assembly::ContextBundle,
    memory::{MemoryEntry, MemoryRole},
    sensing::SensingCandidate,
    usage::ExplorationRunSummary,
};

pub(super) fn working_context(
    messages: &[MemoryEntry],
    runs: &[ExplorationRunSummary],
    candidates: &[SensingCandidate],
    trigger: Option<&ExplorationTrigger>,
    now: DateTime<Utc>,
) -> ContextBundle {
    let mut bundle = ContextBundle::default();
    let wake = match trigger {
        Some(ExplorationTrigger::Manual { .. }) => "Wake reason: the user explicitly requested an exploration cycle.".to_owned(),
        Some(ExplorationTrigger::DeferredFollowUp) => "Wake reason: a deferred follow-up is due. Check whether it remains live against the latest conversation; otherwise remain silent.".to_owned(),
        Some(ExplorationTrigger::Intent(intent)) => format!("Wake reason: explicit evidence-seeking intent. Re-evaluate it against the latest conversation.\n<exploration-intent id=\"{}\" origin=\"{}\">\nquestion: {}\nwhy-now: {}\nsource-revisions: {}\n</exploration-intent>", intent.id, intent.origin.as_str(), intent.question, intent.why_now, intent.source_revision_ids.join(", ")),
        None => "Wake reason: scheduled exploration cycle; no user request is waiting.".to_owned(),
    };
    bundle.include(
        "symbiont.exploration_request",
        "触发条件与最近对话边缘",
        "判断时机；助手回复不是用户偏好",
        format!("{wake}\n{}", conversation_edge(messages, now)),
    );
    let last_user = messages.iter().rposition(|m| m.role == MemoryRole::User);
    for (index, entry) in messages
        .iter()
        .enumerate()
        .filter(|(i, m)| m.role == MemoryRole::User && Some(*i) != last_user)
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let id = entry.revision_id.as_deref().unwrap_or("");
        bundle.include(&format!("symbiont.exploration.user.{}", if id.is_empty() { index.to_string() } else { id.to_owned() }), "本地用户原话", "弱兴趣提示，不是探索结果或永久偏好", json!({"messageId":id,"role":"user","at":entry.at,"content":bounded_message_excerpt(&entry.content, 700)}).to_string());
    }
    let ledger = runs.iter().take(8).map(|run| json!({
        "traceId":run.trace_id, "at":run.completed_at, "surfaced":run.surfaced,
        "topic":run.focus.as_ref().map(|f| f.title.as_str()),
        "queries":run.search_queries.iter().take(3).map(|q| bounded_message_excerpt(q, 160)).collect::<Vec<_>>(),
        "deliveredExcerpt":run.message.as_deref().filter(|_| run.surfaced).map(|m| bounded_message_excerpt(m, 360)),
    })).collect::<Vec<_>>();
    if !ledger.is_empty() {
        bundle.include("symbiont.exploration_avoid", "历史探索投递记录", "仅防重复，不作为选题证据或用户兴趣", json!({"purpose":"Negative repetition evidence only. These are assistant choices, NOT user interests. A silent run means no delivery, not a rejected topic.","runs":ledger}).to_string());
    }
    let mut optional_remaining = 8_000_usize;
    for candidate in candidates {
        // Candidate summary is an intake routing claim, not the received body.
        // Do not repeat proposed_input or possible_connection as corroboration.
        let source = format!("symbiont.exploration.candidate.{}", candidate.id);
        let value = json!({
            "id":candidate.id,"title":candidate.title,"routingClaim":candidate.summary,
            "observedAt":candidate.observed_at,"eventAt":candidate.event_at,"documentAt":candidate.source_document_at,
            "sources":candidate.sources,"status":"unverified intake; open sources to verify; framing deliberately withheld"
        }).to_string();
        let chars = value.chars().count();
        if chars > optional_remaining {
            bundle.defer(
                &source,
                "外部输入候选",
                "本轮线索预算不足，整条未装入；不是摘要、丢弃或拒绝",
            );
        } else {
            optional_remaining -= chars;
            bundle.include(
                &source,
                "外部输入候选",
                "待独立核实的线索；非原文全文或已证实事实",
                value,
            );
        }
    }
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(role: MemoryRole, id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            role,
            at: "2026-09-03T01:00:00Z".into(),
            content: text.into(),
            revision_id: Some(id.into()),
            parts: vec![],
            metadata: None,
            delivery_state: None,
        }
    }
    #[test]
    fn user_hints_do_not_replay_assistant_essays_or_duplicate_the_current_user() {
        let messages = vec![
            message(MemoryRole::User, "u1", "older user question"),
            message(MemoryRole::Assistant, "a1", "stale assistant framing"),
            message(MemoryRole::User, "u2", "latest user question"),
        ];
        let bundle = working_context(&messages, &[], &[], None, Utc::now());
        let text = bundle
            .fragments
            .iter()
            .map(|f| f.value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("older user question"));
        assert!(!text.contains("stale assistant framing"));
        assert_eq!(text.matches("latest user question").count(), 1);
    }
}
