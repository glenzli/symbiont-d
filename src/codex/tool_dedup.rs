use std::collections::HashMap;

use serde_json::{Map, Value};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ToolCallPlan {
    Execute,
    Reuse { original_sequence: u32 },
}

#[derive(Default)]
pub(super) struct TurnToolDeduplicator {
    successful_reads: HashMap<String, u32>,
}

impl TurnToolDeduplicator {
    pub(super) fn plan(&mut self, namespace: &str, tool: &str, arguments: &Value) -> ToolCallPlan {
        if (namespace == "pcp" && is_pcp_mutation(tool))
            || (namespace == "symbiont" && is_symbiont_context_mutation(tool))
        {
            self.successful_reads.clear();
            return ToolCallPlan::Execute;
        }
        if namespace != "pcp" {
            return ToolCallPlan::Execute;
        }
        if !is_cacheable_read(tool) {
            return ToolCallPlan::Execute;
        }

        self.successful_reads
            .get(&call_key(namespace, tool, arguments))
            .copied()
            .map(|original_sequence| ToolCallPlan::Reuse { original_sequence })
            .unwrap_or(ToolCallPlan::Execute)
    }

    pub(super) fn remember_success(
        &mut self,
        namespace: &str,
        tool: &str,
        arguments: &Value,
        sequence: u32,
    ) {
        if namespace == "pcp" && is_cacheable_read(tool) {
            self.successful_reads
                .insert(call_key(namespace, tool, arguments), sequence);
        }
    }
}

fn is_cacheable_read(tool: &str) -> bool {
    matches!(tool, "search_pages" | "read_pages")
}

fn is_pcp_mutation(tool: &str) -> bool {
    matches!(
        tool,
        "write_summary" | "write_page" | "revise_page" | "link_pages"
    )
}

fn is_symbiont_context_mutation(tool: &str) -> bool {
    matches!(
        tool,
        "complete_orientation"
            | "revise_orientation"
            | "update_current_map"
            | "update_open_loops"
            | "record_profile_review"
            | "open_hunch"
            | "revise_hunch"
            | "retire_hunch"
            | "upsert_episode"
            | "upsert_interaction_hypothesis"
            | "schedule_follow_up"
            | "complete_reflection"
    )
}

fn call_key(namespace: &str, tool: &str, arguments: &Value) -> String {
    let canonical_arguments = canonicalize(arguments);
    format!(
        "{namespace}\n{tool}\n{}",
        serde_json::to_string(&canonical_arguments).unwrap_or_default()
    )
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize(&values[key]));
            }
            Value::Object(canonical)
        }
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ToolCallPlan, TurnToolDeduplicator};

    #[test]
    fn reuses_an_identical_successful_read_with_reordered_arguments() {
        let mut deduplicator = TurnToolDeduplicator::default();
        let first = json!({"query": "shadow", "limit": 10, "mode": "text"});
        let reordered = json!({"mode": "text", "query": "shadow", "limit": 10});

        assert_eq!(
            deduplicator.plan("pcp", "search_pages", &first),
            ToolCallPlan::Execute
        );
        deduplicator.remember_success("pcp", "search_pages", &first, 2);
        assert_eq!(
            deduplicator.plan("pcp", "search_pages", &reordered),
            ToolCallPlan::Reuse {
                original_sequence: 2
            }
        );
    }

    #[test]
    fn changed_arguments_are_a_new_read() {
        let mut deduplicator = TurnToolDeduplicator::default();
        let first = json!({"query": "shadow", "limit": 10});
        deduplicator.remember_success("pcp", "search_pages", &first, 0);

        assert_eq!(
            deduplicator.plan(
                "pcp",
                "search_pages",
                &json!({"query": "shadow", "limit": 5})
            ),
            ToolCallPlan::Execute
        );
    }

    #[test]
    fn a_pcp_mutation_invalidates_prior_reads() {
        let mut deduplicator = TurnToolDeduplicator::default();
        let search = json!({"query": "shadow"});
        deduplicator.remember_success("pcp", "search_pages", &search, 0);

        assert_eq!(
            deduplicator.plan(
                "pcp",
                "write_page",
                &json!({"content": "new project state"})
            ),
            ToolCallPlan::Execute
        );
        assert_eq!(
            deduplicator.plan("pcp", "search_pages", &search),
            ToolCallPlan::Execute
        );
    }

    #[test]
    fn a_symbiont_hunch_mutation_invalidates_prior_pcp_reads() {
        let mut deduplicator = TurnToolDeduplicator::default();
        let search = json!({"query": "conversation trigger"});
        deduplicator.remember_success("pcp", "search_pages", &search, 0);

        assert_eq!(
            deduplicator.plan(
                "symbiont",
                "open_hunch",
                &json!({"question": "Could conversation wake exploration?"})
            ),
            ToolCallPlan::Execute
        );
        assert_eq!(
            deduplicator.plan("pcp", "search_pages", &search),
            ToolCallPlan::Execute
        );
    }
}
