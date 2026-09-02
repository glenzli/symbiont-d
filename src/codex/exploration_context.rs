//! A reviewer receives the selected finding in its task input, not the entire
//! scout prompt a second time. Deferred sections remain traceable in the audit.
use super::autonomous::ExplorationScoutFinding;
use crate::context_assembly::ContextBundle;

pub(super) fn review_context(
    source: &ContextBundle,
    finding: &ExplorationScoutFinding,
) -> ContextBundle {
    let mut result = ContextBundle::default();
    for row in &source.selection {
        let selected_anchor = row
            .source
            .strip_prefix("symbiont.exploration.user.")
            .is_some_and(|id| {
                finding
                    .source_revision_ids
                    .iter()
                    .any(|source| source == id)
            });
        let include = row.included
            && (selected_anchor
                || matches!(
                    row.source.as_str(),
                    "symbiont.memory_boundary"
                        | "symbiont.autonomy"
                        | "symbiont.exploration_request"
                        | "symbiont.exploration_avoid"
                ));
        if include && let Some(fragment) = source.fragments.iter().find(|f| f.source == row.source)
        {
            result.include(
                &row.source,
                &row.origin,
                &row.purpose,
                fragment.value.clone(),
            );
        } else {
            let mut deferred = row.clone();
            deferred.included = false;
            deferred.purpose = "侦察方向/未选候选不重复装入复核；按明确缺口读取原始来源".into();
            result.selection.push(deferred);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_admits_only_selected_anchors_and_action_boundaries() {
        let mut source = ContextBundle::default();
        for name in [
            "symbiont.memory_boundary",
            "symbiont.exploration_request",
            "symbiont.exploration_avoid",
            "symbiont.background.map",
            "symbiont.exploration.user.u1",
            "symbiont.exploration.user.u2",
            "symbiont.exploration.candidate.c1",
        ] {
            source.include(name, "test", "test", name.into());
        }
        let finding = ExplorationScoutFinding {
            topic: "topic".into(),
            claim: "claim".into(),
            evidence: vec![],
            connection_hypothesis: "hypothesis".into(),
            strongest_counterpoint: "counterpoint".into(),
            source_revision_ids: vec!["u1".into()],
            related_hunch_revision_ids: vec![],
        };
        let review = review_context(&source, &finding);
        assert_eq!(review.fragments.len(), 4);
        assert!(
            review
                .fragments
                .iter()
                .any(|f| f.source.ends_with("user.u1"))
        );
        assert!(
            !review
                .fragments
                .iter()
                .any(|f| f.source.ends_with("user.u2"))
        );
        assert_eq!(review.selection.len(), source.selection.len());
        assert!(review.selection.iter().filter(|r| !r.included).count() == 3);
    }
}
