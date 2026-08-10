use std::sync::Arc;

use anyhow::{Context, Result};
use pcp_runtime::{MaintenanceWorkerRequest, MaintenanceWorkerResponse};
use sha2::{Digest, Sha256};

use super::{
    ReconciliationDependencies, ReconciliationMode, ReconciliationProposal,
    ReconciliationProposalKind, ReconciliationStore, store::CompletedRun, worker::over_budget,
};
use crate::{inference::InferenceAttempt, profile::SetupStatus};

#[derive(Clone)]
pub(super) struct PcpMaintenanceWorker {
    store: Arc<ReconciliationStore>,
    dependencies: ReconciliationDependencies,
}

impl PcpMaintenanceWorker {
    pub(super) fn new(
        store: Arc<ReconciliationStore>,
        dependencies: ReconciliationDependencies,
    ) -> Self {
        Self {
            store,
            dependencies,
        }
    }

    pub(super) async fn evaluate(
        &self,
        request: MaintenanceWorkerRequest,
    ) -> Result<MaintenanceWorkerResponse> {
        let profile = self.dependencies.profile.snapshot().await;
        if profile.status != SetupStatus::Ready {
            return Ok(MaintenanceWorkerResponse::Defer {
                reason: Some("symbiont setup is incomplete".to_owned()),
            });
        }
        if over_budget(&self.dependencies).await? {
            return Ok(MaintenanceWorkerResponse::Defer {
                reason: Some("background analysis token budget is exhausted".to_owned()),
            });
        }
        let Some(input_events) = self
            .dependencies
            .conversation
            .subscribe_background_input()
            .await
        else {
            return Ok(MaintenanceWorkerResponse::Defer {
                reason: Some("a user conversation is active".to_owned()),
            });
        };
        let encoded = serde_json::to_vec(&request).context("encode PCP maintenance request")?;
        let digest = format!("{:x}", Sha256::digest(&encoded));
        let run_id = self
            .store
            .start_run(
                ReconciliationMode::Preview,
                "pcp_runtime",
                digest,
                request_page_count(&request),
                None,
            )
            .await?;
        let (response, invocations, interrupted) = match self
            .dependencies
            .inference
            .evaluate_pcp_maintenance(&request, input_events.clone())
            .await
        {
            InferenceAttempt::Completed(outcome) => {
                (outcome.value, outcome.invocations, outcome.interrupted)
            }
            InferenceAttempt::Deferred {
                reason,
                invocations,
            } => {
                tracing::warn!(
                    target: crate::runtime_log::TARGET,
                    event = "generic_inference_deferred",
                    task = "pcp_maintenance",
                    reason,
                    "PCP semantic maintenance was deferred without invoking Codex"
                );
                let interrupted = input_events.has_changed().unwrap_or(true);
                (
                    MaintenanceWorkerResponse::Defer {
                        reason: Some(if interrupted {
                            "superseded by newer user input".to_owned()
                        } else {
                            "semantic worker temporarily unavailable".to_owned()
                        }),
                    },
                    invocations,
                    interrupted,
                )
            }
        };
        self.dependencies.usage.record_all(&invocations).await?;
        if interrupted {
            self.store.interrupt_run(&run_id).await?;
            return Ok(response);
        }

        let trace_id = invocations.first().map(|item| item.id.clone());
        let model = invocations
            .last()
            .map(|item| item.model_display_name.clone());
        let total_tokens = invocations.iter().map(|item| item.total_tokens).sum();
        let summary = response_summary(&request, &response);
        let proposals = response_proposals(&request, &response);
        self.store
            .complete_run(
                &run_id,
                CompletedRun {
                    summary: Some(summary),
                    proposals,
                    actions: Vec::new(),
                    trace_id,
                    model,
                    total_tokens,
                },
            )
            .await?;
        Ok(response)
    }
}

fn request_page_count(request: &MaintenanceWorkerRequest) -> usize {
    match request {
        MaintenanceWorkerRequest::SummarizePage { .. } => 1,
        MaintenanceWorkerRequest::SelectConsolidation { pages, .. } => pages.len(),
        MaintenanceWorkerRequest::ConsolidatePages { pages } => pages.len(),
        MaintenanceWorkerRequest::SelectRetentionMilestones { pages, .. } => pages.len(),
    }
}

fn request_revision_ids(request: &MaintenanceWorkerRequest) -> Vec<String> {
    match request {
        MaintenanceWorkerRequest::SummarizePage { page } => vec![page.revision_id.clone()],
        MaintenanceWorkerRequest::SelectConsolidation { pages, .. } => {
            pages.iter().map(|page| page.revision_id.clone()).collect()
        }
        MaintenanceWorkerRequest::ConsolidatePages { pages } => {
            pages.iter().map(|page| page.revision_id.clone()).collect()
        }
        MaintenanceWorkerRequest::SelectRetentionMilestones { pages, .. } => {
            pages.iter().map(|page| page.revision_id.clone()).collect()
        }
    }
}

fn selected_revision_ids(request: &MaintenanceWorkerRequest, page_ids: &[String]) -> Vec<String> {
    match request {
        MaintenanceWorkerRequest::SelectConsolidation { pages, .. } => pages
            .iter()
            .filter(|page| page_ids.contains(&page.page_id))
            .map(|page| page.revision_id.clone())
            .collect(),
        _ => request_revision_ids(request),
    }
}

fn response_summary(
    request: &MaintenanceWorkerRequest,
    response: &MaintenanceWorkerResponse,
) -> String {
    let operation = match request {
        MaintenanceWorkerRequest::SummarizePage { .. } => "摘要索引判断",
        MaintenanceWorkerRequest::SelectConsolidation { .. } => "重复候选筛选",
        MaintenanceWorkerRequest::ConsolidatePages { .. } => "合并内容复核",
        MaintenanceWorkerRequest::SelectRetentionMilestones { .. } => "长期里程碑判断",
    };
    let decision = match response {
        MaintenanceWorkerResponse::WriteSummary { .. } => "建议建立 Summary",
        MaintenanceWorkerResponse::Candidate { rationale, .. } => {
            return format!(
                "{operation}：发现合并候选{}",
                optional_reason(rationale.as_deref())
            );
        }
        MaintenanceWorkerResponse::Consolidate { .. } => "建议合并",
        MaintenanceWorkerResponse::Retain { milestones } => {
            return format!(
                "{operation}：建议阶段性保留 {} 个 Revision",
                milestones.len()
            );
        }
        MaintenanceWorkerResponse::KeepSeparate { reason } => {
            return format!(
                "{operation}：保留现状{}",
                optional_reason(reason.as_deref())
            );
        }
        MaintenanceWorkerResponse::NoCandidate { reason } => {
            return format!(
                "{operation}：没有候选{}",
                optional_reason(reason.as_deref())
            );
        }
        MaintenanceWorkerResponse::Defer { reason } => {
            return format!("{operation}：暂缓{}", optional_reason(reason.as_deref()));
        }
    };
    format!("{operation}：{decision}")
}

fn optional_reason(reason: Option<&str>) -> String {
    reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| format!("，{reason}"))
        .unwrap_or_default()
}

fn response_proposals(
    request: &MaintenanceWorkerRequest,
    response: &MaintenanceWorkerResponse,
) -> Vec<ReconciliationProposal> {
    match response {
        MaintenanceWorkerResponse::WriteSummary { .. } => vec![ReconciliationProposal {
            action: ReconciliationProposalKind::Resummarize,
            subject: "为长 Page 建立路由摘要".to_owned(),
            reason: "模型认为该内容值得进入稀疏 Summary 索引。".to_owned(),
            revision_ids: request_revision_ids(request),
        }],
        MaintenanceWorkerResponse::Candidate {
            page_ids,
            rationale,
        } => vec![ReconciliationProposal {
            action: ReconciliationProposalKind::Consolidate,
            subject: "复核潜在重复 Page".to_owned(),
            reason: rationale
                .clone()
                .unwrap_or_else(|| "模型在路由索引中发现了潜在重复。".to_owned()),
            revision_ids: selected_revision_ids(request, page_ids),
        }],
        MaintenanceWorkerResponse::Consolidate { .. } => vec![ReconciliationProposal {
            action: ReconciliationProposalKind::Consolidate,
            subject: "以单一 canonical Page 吸收重复内容".to_owned(),
            reason: "模型读取 Detail 后仍认为合并不会丢失独立语义。".to_owned(),
            revision_ids: request_revision_ids(request),
        }],
        MaintenanceWorkerResponse::Retain { milestones } => milestones
            .iter()
            .map(|milestone| ReconciliationProposal {
                action: ReconciliationProposalKind::Retain,
                subject: "阶段性保留精确 Revision".to_owned(),
                reason: milestone.reason.clone(),
                revision_ids: vec![milestone.revision_id.clone()],
            })
            .collect(),
        MaintenanceWorkerResponse::KeepSeparate { .. }
        | MaintenanceWorkerResponse::NoCandidate { .. }
        | MaintenanceWorkerResponse::Defer { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcp_runtime::MaintenanceDetailPage;

    #[test]
    fn consolidation_decision_becomes_a_bounded_visible_proposal() {
        let request = MaintenanceWorkerRequest::ConsolidatePages {
            pages: vec![detail("rev-a"), detail("rev-b")],
        };
        let response = MaintenanceWorkerResponse::Consolidate {
            canonical_page_id: "rev-a".to_owned(),
            content: "one durable state".to_owned(),
        };

        let proposals = response_proposals(&request, &response);

        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].revision_ids, vec!["rev-a", "rev-b"]);
    }

    fn detail(page_id: &str) -> MaintenanceDetailPage {
        MaintenanceDetailPage {
            page_id: format!("page-{page_id}"),
            revision_id: page_id.to_owned(),
            namespace: "project:test".to_owned(),
            created_at: "2026-08-04T00:00:00Z".to_owned(),
            observed_at: None,
            media_type: Some("text/markdown".to_owned()),
            content: Some("detail".to_owned()),
            summary: None,
            facets: None,
            source_refs: Vec::new(),
            relations: Vec::new(),
        }
    }
}
