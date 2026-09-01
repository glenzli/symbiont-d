use serde::{Deserialize, Serialize};

pub const NATIVE_THREAD_ROLLOVER_PERCENT: u64 = 35;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadContextPressure {
    pub input_tokens: u64,
    pub context_window: u64,
}

#[derive(Clone, Debug)]
pub struct NativeThreadCursor {
    revision_id: Option<String>,
    needs_bridge: bool,
}

impl NativeThreadCursor {
    pub fn new() -> Self {
        Self {
            revision_id: None,
            needs_bridge: true,
        }
    }

    pub fn revision(&self) -> Option<&str> {
        (!self.needs_bridge)
            .then_some(self.revision_id.as_deref())
            .flatten()
    }

    pub fn needs_bridge(&self) -> bool {
        self.needs_bridge
    }

    pub fn bridge_completed(&mut self) {
        self.needs_bridge = false;
    }

    pub fn mark(&mut self, revision_id: String) {
        if !self.needs_bridge {
            self.revision_id = Some(revision_id);
        }
    }

    pub fn rotate(&mut self) {
        self.revision_id = None;
        self.needs_bridge = true;
    }
}

#[derive(Clone, Debug)]
pub struct RolloverDecision {
    reason: RolloverReason,
    conversation_scope: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RolloverReason {
    ContextPressure,
    NativeCompaction,
}

impl RolloverDecision {
    pub fn reason(&self) -> &'static str {
        match self.reason {
            RolloverReason::ContextPressure => "context_pressure",
            RolloverReason::NativeCompaction => "native_compaction",
        }
    }

    pub fn prompt(&self) -> String {
        let reason = match self.reason {
            RolloverReason::ContextPressure => "the native context reached its rollover boundary",
            RolloverReason::NativeCompaction => "Codex already compacted the native context",
        };
        format!(
            "Native thread rollover follows this turn because {reason}. This is context-compression \
             pressure, not proof that durable memory should be written. The Host will bridge exact \
             recent Source Pages into the next thread. Before the final reply, write at most one \
             PCP Page in `{}` only if the underlying discussion independently contains durable \
             state such as a decision, correction, stable constraint, unresolved long-running \
             question, or meaningful recurrence. Preserve exact `source_message_ids`; do not create \
             a generic conversation checkpoint or compression summary. Then answer normally.",
            self.conversation_scope
        )
    }
}

pub fn decide(
    pressure: Option<&ThreadContextPressure>,
    native_compactions: u64,
    conversation_scope: &str,
) -> Option<RolloverDecision> {
    let reason = if native_compactions > 0 {
        Some(RolloverReason::NativeCompaction)
    } else if pressure.is_some_and(context_boundary_reached) {
        Some(RolloverReason::ContextPressure)
    } else {
        None
    }?;
    Some(RolloverDecision {
        reason,
        conversation_scope: conversation_scope.to_owned(),
    })
}

fn context_boundary_reached(pressure: &ThreadContextPressure) -> bool {
    pressure.context_window > 0
        && pressure.input_tokens.saturating_mul(100)
            >= pressure
                .context_window
                .saturating_mul(NATIVE_THREAD_ROLLOVER_PERCENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_over_at_the_context_boundary() {
        let below = ThreadContextPressure {
            input_tokens: 34_999,
            context_window: 100_000,
        };
        let boundary = ThreadContextPressure {
            input_tokens: 35_000,
            context_window: 100_000,
        };
        assert!(decide(Some(&below), 0, "conversation:test").is_none());
        assert!(decide(Some(&boundary), 0, "conversation:test").is_some());
    }

    #[test]
    fn native_compaction_forces_rollover() {
        let decision = decide(None, 1, "conversation:test").expect("rollover");
        assert_eq!(decision.reason(), "native_compaction");
        assert!(decision.prompt().contains("already compacted"));
        assert!(decision.prompt().contains("not proof"));
        assert!(decision.prompt().contains("do not create"));
    }

    #[test]
    fn a_rotated_thread_requires_an_exact_bridge_before_advancing_its_cursor() {
        let mut cursor = NativeThreadCursor::new();
        cursor.mark("rev_before_first_bridge".to_owned());
        assert!(cursor.revision().is_none());

        cursor.bridge_completed();
        cursor.mark("rev_1".to_owned());
        assert_eq!(cursor.revision(), Some("rev_1"));

        cursor.rotate();
        cursor.mark("rev_not_in_new_thread".to_owned());
        assert!(cursor.needs_bridge());
        assert!(cursor.revision().is_none());

        cursor.bridge_completed();
        cursor.mark("rev_2".to_owned());
        assert_eq!(cursor.revision(), Some("rev_2"));
    }
}
