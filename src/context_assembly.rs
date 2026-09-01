//! Task-scoped context and its diagnostic provenance. Selection metadata is
//! retained for inspection, never smuggled into the model's evidence text.
use serde::{Deserialize, Serialize};

use crate::diagnostics::ContextFragment;

#[derive(Clone, Debug, Default)]
pub struct ContextBundle {
    pub fragments: Vec<ContextFragment>,
    pub selection: Vec<ContextSelection>,
}

pub fn audit_fragments(
    fragments: &[ContextFragment],
    selection: &[ContextSelection],
) -> Vec<ContextSelection> {
    let mut audit = selection.to_vec();
    for row in &mut audit {
        if row.included && !fragments.iter().any(|part| part.source == row.source) {
            row.included = false;
            row.purpose = "已在当前输入或最近对话桥接中提供，不重复装入".into();
        }
    }
    for part in fragments {
        if audit
            .iter()
            .any(|row| row.source == part.source && row.included)
        {
            continue;
        }
        let (origin, purpose) = match part.source.as_str() {
            "symbiont.time" => ("宿主时钟", "当前时间"),
            "symbiont.compute" => ("宿主计算配置", "当前模型级别与升级边界"),
            "symbiont.profile" => (
                "本地用户确认的 Orientation",
                "稳定身份与偏好，不是后台互动假说",
            ),
            "symbiont.working_context" => ("本地聊天记录", "原生线程尚未包含的最近对话"),
            "symbiont.rollover" => ("宿主线程压力判断", "线程轮换提示"),
            "symbiont.interaction" => ("宿主交互协议", "本轮输出约定"),
            _ => ("宿主应用上下文", "当前任务提供"),
        };
        audit.push(ContextSelection {
            source: part.source.clone(),
            origin: origin.into(),
            purpose: purpose.into(),
            included: true,
            chars: part.value.chars().count(),
        });
    }
    audit
}

/// Bound optional evidence after all fragments (including the history bridge)
/// are assembled. Never truncate a policy, user input, or a structured record.
pub fn budget_recall(
    fragments: &mut Vec<ContextFragment>,
    audit: &mut [ContextSelection],
    budget: usize,
) {
    let mut size = fragments
        .iter()
        .map(|part| part.value.chars().count())
        .sum::<usize>();
    for index in (0..fragments.len()).rev() {
        if size <= budget {
            break;
        }
        if !fragments[index].source.starts_with("symbiont.transcript.")
            && !fragments[index].source.starts_with("symbiont.pcp.")
        {
            continue;
        }
        let removed = fragments.remove(index);
        size -= removed.value.chars().count();
        if let Some(row) = audit.iter_mut().find(|row| row.source == removed.source) {
            row.included = false;
            row.purpose = "最终上下文预算：本轮未装入，仍可按来源标识读取".into();
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSelection {
    pub source: String,
    pub origin: String,
    pub purpose: String,
    pub included: bool,
    pub chars: usize,
}

impl ContextBundle {
    pub fn single(source: &str, origin: &str, purpose: &str, value: String) -> Self {
        let mut bundle = Self::default();
        bundle.include(source, origin, purpose, value);
        bundle
    }

    pub fn include(&mut self, source: &str, origin: &str, purpose: &str, value: String) {
        if value.trim().is_empty() {
            return;
        }
        assert!(
            !self.fragments.iter().any(|part| part.source == source),
            "duplicate context source: {source}"
        );
        self.selection.push(ContextSelection {
            source: source.to_owned(),
            origin: origin.to_owned(),
            purpose: purpose.to_owned(),
            included: true,
            chars: value.chars().count(),
        });
        self.fragments.push(ContextFragment {
            source: source.to_owned(),
            kind: "application".to_owned(),
            value,
        });
    }

    pub fn defer(&mut self, source: &str, origin: &str, reason: &str) {
        self.selection.push(ContextSelection {
            source: source.to_owned(),
            origin: origin.to_owned(),
            purpose: reason.to_owned(),
            included: false,
            chars: 0,
        });
    }

    pub fn extend(&mut self, other: Self) {
        for fragment in &other.fragments {
            assert!(
                !self
                    .fragments
                    .iter()
                    .any(|part| part.source == fragment.source),
                "duplicate context source"
            );
        }
        self.fragments.extend(other.fragments);
        self.selection.extend(other.selection);
    }

    pub fn defer_background(&mut self) {
        for (source, origin) in [
            (
                "symbiont.background.map",
                "本地工作地图、开放问题、画像审阅",
            ),
            ("symbiont.background.curiosity", "本地探索问题与反馈队列"),
            (
                "symbiont.background.reflection",
                "本地互动事件、主题片段与暂定假说",
            ),
            ("symbiont.background.compute_policies", "宿主计算路由规则"),
        ] {
            self.defer(
                source,
                origin,
                "普通对话不预装后台台账；需要时用 read_background_context 读取。路由由宿主执行。",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_omissions_are_audit_only() {
        let mut bundle = ContextBundle::single(
            "symbiont.memory_boundary",
            "host",
            "policy",
            "writable scope".into(),
        );
        bundle.defer_background();
        assert_eq!(bundle.fragments.len(), 1);
        assert_eq!(
            bundle
                .selection
                .iter()
                .filter(|part| !part.included)
                .count(),
            4
        );
        assert!(
            bundle
                .fragments
                .iter()
                .all(|part| !part.value.contains("假说"))
        );
    }

    #[test]
    fn budget_drops_whole_optional_records_not_core_instructions() {
        let mut bundle = ContextBundle::single(
            "symbiont.memory_boundary",
            "host",
            "scope",
            "权限边界".repeat(10),
        );
        bundle.include("symbiont.pcp.rev_a", "pcp", "match", "原始证据".repeat(100));
        bundle.include(
            "symbiont.working_context",
            "local",
            "continuity",
            "当前对话".repeat(10),
        );
        let mut audit = audit_fragments(&bundle.fragments, &bundle.selection);
        budget_recall(&mut bundle.fragments, &mut audit, 100);
        assert_eq!(bundle.fragments.len(), 2);
        assert!(
            bundle
                .fragments
                .iter()
                .all(|part| part.value.chars().count() == 40)
        );
        assert!(
            !audit
                .iter()
                .find(|row| row.source == "symbiont.pcp.rev_a")
                .unwrap()
                .included
        );
    }
}
