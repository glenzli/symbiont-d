use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{autonomy::AutonomyConfig, usage::UsageHeadline};

pub const PROPOSE_OUTREACH_TOOL: &str = "propose_proactive_message";

/// How an autonomous message should enter the user's attention.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutreachKind {
    Intervention,
    Note,
}

impl OutreachKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intervention => "intervention",
            Self::Note => "note",
        }
    }

    pub fn from_arguments(arguments: &Value) -> Self {
        match arguments.get("kind").and_then(Value::as_str) {
            Some("note") => Self::Note,
            _ => Self::Intervention,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutreachCandidate {
    pub kind: OutreachKind,
    pub message: String,
    pub reason: String,
    pub source_revision_ids: Vec<String>,
}

impl OutreachCandidate {
    pub fn from_tool_arguments(arguments: &Value) -> Option<Self> {
        let message = arguments
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_owned();
        let reason = arguments
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_owned();
        let source_revision_ids = arguments
            .get("source_revision_ids")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        (!source_revision_ids.is_empty()).then_some(Self {
            kind: OutreachKind::from_arguments(arguments),
            message,
            reason,
            source_revision_ids,
        })
    }
}

pub fn has_budget(kind: OutreachKind, config: &AutonomyConfig, usage: &UsageHeadline) -> bool {
    match kind {
        OutreachKind::Intervention => {
            usage.autonomous_interventions_today < config.daily_interrupt_limit as u64
        }
        OutreachKind::Note => usage.autonomous_notes_today < config.daily_note_limit as u64,
    }
}

pub fn all_budgets_exhausted(config: &AutonomyConfig, usage: &UsageHeadline) -> bool {
    !has_budget(OutreachKind::Intervention, config, usage)
        && !has_budget(OutreachKind::Note, config, usage)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_candidates_remain_interventions() {
        let candidate = OutreachCandidate::from_tool_arguments(&json!({
            "message": "A durable change.",
            "reason": "It changes the current decision.",
            "source_revision_ids": ["rev_1"]
        }))
        .expect("candidate");

        assert_eq!(candidate.kind, OutreachKind::Intervention);
    }

    #[test]
    fn note_budget_does_not_consume_intervention_budget() {
        let config = AutonomyConfig {
            daily_interrupt_limit: 1,
            daily_note_limit: 2,
            ..AutonomyConfig::default()
        };
        let usage = UsageHeadline {
            autonomous_interventions_today: 1,
            autonomous_notes_today: 1,
            ..UsageHeadline::default()
        };

        assert!(!has_budget(OutreachKind::Intervention, &config, &usage));
        assert!(has_budget(OutreachKind::Note, &config, &usage));
        assert!(!all_budgets_exhausted(&config, &usage));
    }
}
