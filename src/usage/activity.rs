//! Stable semantic classification for recorded model invocations.
//!
//! `origin` remains an execution detail: it selects a prompt, tool surface, or
//! Codex thread. Product surfaces and accounting must instead classify what
//! the invocation did in the system, independently of a particular model or
//! input adapter.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvocationActivity {
    pub activity: &'static str,
    pub stage: &'static str,
    pub input_source: Option<&'static str>,
}

impl InvocationActivity {
    pub fn from_origin(origin: &str) -> Self {
        match origin {
            "interactive" => Self::conversation("reply"),
            "continuation" => Self::conversation("continuation"),
            "ambient_sense" => Self::sensing("external"),
            "luna_sense" => Self::sensing("luna"),
            "ambient_dedup" => Self::exploration("deduplicate"),
            "ambient_review" => Self::exploration("review"),
            "autonomous_scout" => Self::exploration("scout"),
            "autonomous" => Self::exploration("review"),
            "attacker" => Self::exploration("challenge"),
            "reflection" => Self::reflection("organize"),
            "maintenance" => Self::maintenance("context"),
            "pcp_maintenance" => Self::maintenance("pcp"),
            "reconciliation_preview" => Self::maintenance("reconciliation_preview"),
            "reconciliation_apply" => Self::maintenance("reconciliation_apply"),
            _ => Self::maintenance("internal"),
        }
    }

    const fn conversation(stage: &'static str) -> Self {
        Self {
            activity: "conversation",
            stage,
            input_source: None,
        }
    }

    const fn sensing(input_source: &'static str) -> Self {
        Self {
            activity: "sensing",
            stage: "sense",
            input_source: Some(input_source),
        }
    }

    const fn exploration(stage: &'static str) -> Self {
        Self {
            activity: "exploration",
            stage,
            input_source: None,
        }
    }

    const fn reflection(stage: &'static str) -> Self {
        Self {
            activity: "reflection",
            stage,
            input_source: None,
        }
    }

    const fn maintenance(stage: &'static str) -> Self {
        Self {
            activity: "maintenance",
            stage,
            input_source: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InvocationActivity;

    #[test]
    fn classifies_input_adapters_as_one_sensing_stage() {
        let luna = InvocationActivity::from_origin("luna_sense");
        let external = InvocationActivity::from_origin("ambient_sense");
        assert_eq!(luna.activity, "sensing");
        assert_eq!(external.activity, "sensing");
        assert_eq!(luna.stage, external.stage);
        assert_eq!(luna.input_source, Some("luna"));
        assert_eq!(external.input_source, Some("external"));
    }

    #[test]
    fn classifies_local_duplicate_work_as_an_exploration_stage() {
        let duplicate = InvocationActivity::from_origin("ambient_dedup");
        assert_eq!(duplicate.activity, "exploration");
        assert_eq!(duplicate.stage, "deduplicate");
        assert_eq!(duplicate.input_source, None);
    }
}
