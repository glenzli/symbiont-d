//! Foreground-only joint admission of durable and raw evidence. Retrieval and
//! background recurrence keep their original data. Ranking selects records;
//! neither a vector score nor provenance alone establishes semantic coverage.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use infer_runtime_client::{RetrievalRerankRequest, RetrievalRerankResponse, RetrievalTextInput};
use pcp_core::ContextPackEntry;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    CompoundContext, ContinuityHost,
    recall_sources::{self, Lineage},
};
use crate::{
    context_assembly::{ContextBundle, RecallCandidateAudit},
    memory::MemoryRole,
    transcript::TranscriptSearchMessage,
};

const MAX_PCP_RECORDS: usize = 3;
const MAX_LOCAL_RECORDS: usize = 4;
const RECALL_CHARS: usize = 9_000;
const RANK_TIMEOUT: Duration = Duration::from_secs(8);
// An admission heuristic for this exact reranker contract, not a probability
// of truth/coverage. Unknown score contracts fail back to lexical admission.
const SCORE_SEMANTICS: &str = "model_relevance_not_calibrated_probability";

struct Candidate {
    source: String,
    content: String,
    origin: String,
    payload: Value,
    pcp: Option<ContextPackEntry>,
    message: Option<TranscriptSearchMessage>,
}

impl Candidate {
    fn is_user(&self) -> bool {
        self.message
            .as_ref()
            .is_some_and(|m| matches!(m.role, MemoryRole::User))
    }
}

fn candidates(context: &CompoundContext) -> (Vec<Candidate>, ContextBundle) {
    let mut bundle = ContextBundle::default();
    bundle.include("symbiont.recall_status", "宿主自动召回", "仅提供可用性与原文寻址格式", json!({
        "pcp": if context.durable_available { "available" } else { "unavailable_not_a_miss" },
        "local": if context.local_available { "available" } else { "unavailable" },
        "localSourceRef": {"providerId": "symbiont:transcript", "locatorTemplate": format!("store/{}/message/{{id}}", context.source_store_id)},
    }).to_string());
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in context.durable.iter().flat_map(|r| &r.entries) {
        let source = format!("symbiont.pcp.{}", entry.revision_id);
        if !seen.insert(source.clone()) {
            continue;
        }
        let origin = format!("PCP · Scope {} · Page {}", entry.namespace, entry.page_id);
        let Some(content) = entry.content.as_ref().filter(|c| !c.trim().is_empty()) else {
            bundle.defer(
                &source,
                &origin,
                "仅命中引用，无正文；留在候选记录中，可按 Revision 读取",
            );
            continue;
        };
        // Scores, ranks and derivation graphs belong in the inspector. Identity,
        // validity and excerpt boundaries remain model-visible evidence.
        let payload = json!({"pageId":entry.page_id,"revisionId":entry.revision_id,
            "scope":entry.namespace,"kind":entry.kind,"content":content,
            "detail":entry.detail,"sourceSpan":entry.source_span,
            "sourceProjectionTruncated":entry.source_projection_truncated,"validity":entry.validity});
        result.push(Candidate {
            source,
            content: content.clone(),
            origin,
            payload,
            pcp: Some(entry.clone()),
            message: None,
        });
    }
    for message in context
        .local
        .iter()
        .flat_map(|r| &r.clusters)
        .flat_map(|c| &c.messages)
    {
        let source = format!("symbiont.transcript.{}", message.message_id);
        if !seen.insert(source.clone()) {
            continue;
        }
        result.push(Candidate {
            source,
            content: message.content.clone(),
            origin: format!(
                "本地聊天 · {} · {}",
                if matches!(message.role, MemoryRole::User) {
                    "用户原话"
                } else {
                    "助手输出（非用户陈述）"
                },
                message.occurred_at
            ),
            payload: json!({"id":message.message_id,"role":message.role,"at":message.occurred_at,
                "content":message.content,"truncated":message.truncated,"matched":message.matched}),
            pcp: None,
            message: Some(message.clone()),
        });
    }
    (result, bundle)
}

pub(super) async fn select(context: &CompoundContext, host: &ContinuityHost) -> ContextBundle {
    let (candidates, bundle) = candidates(context);
    let roots = candidates
        .iter()
        .filter_map(|c| c.pcp.as_ref().map(|p| p.revision_id.clone()))
        .collect::<Vec<_>>();
    let ((ranking, ranking_ms), (lineages, source_calls)) = tokio::join!(
        async {
            let start = Instant::now();
            let result = rank(host.recall_runtime.as_deref(), &context.query, &candidates).await;
            (result, start.elapsed().as_millis() as u64)
        },
        recall_sources::load(host, &roots)
    );
    let (scores, ranker, ranking_error) = match ranking {
        Ok(scores) => (scores, "local_semantic_rerank", None),
        Err(error) => (
            lexical_scores(&context.query, &candidates),
            "lexical_fallback_reranker_unavailable",
            Some(error.to_string().chars().take(320).collect()),
        ),
    };
    let mut bundle = admit(context, candidates, bundle, &scores, &lineages, ranker);
    if let Some(audit) = &mut bundle.recall {
        audit.ranking_duration_ms = ranking_ms;
        audit.ranking_error = ranking_error;
        audit.source_read_calls = source_calls;
    }
    bundle
}

#[cfg(test)]
pub(super) fn fallback(context: &CompoundContext) -> ContextBundle {
    let (candidates, bundle) = candidates(context);
    let scores = lexical_scores(&context.query, &candidates);
    admit(
        context,
        candidates,
        bundle,
        &scores,
        &BTreeMap::new(),
        "lexical_fallback",
    )
}

async fn rank(
    runtime: Option<&crate::infer_runtime::InferRuntimeAccess>,
    query: &str,
    candidates: &[Candidate],
) -> Result<BTreeMap<String, f32>> {
    if candidates.is_empty() {
        return Ok(BTreeMap::new());
    }
    let runtime = runtime.context("no local reranker")?;
    let request = rank_request(query, candidates);
    // Bound padded batch size, not only number of candidates: the local worker
    // pads to its longest document. Ranking text is disposable; evidence isn't.
    tokio::time::timeout(RANK_TIMEOUT, async {
        let client = runtime.client().await?;
        let mut scores = BTreeMap::new();
        for batch in request.candidates.chunks(4) {
            let mut request = request.clone();
            request.candidates = batch.to_vec();
            let response = client.sdk().rerank(&request).await?;
            scores.extend(validate_ranking(&request, response)?);
        }
        Ok(scores)
    })
    .await
    .context("local recall ranking timed out")?
}

fn rank_request(query: &str, candidates: &[Candidate]) -> RetrievalRerankRequest {
    let query_revision = format!("sha256:{:x}", Sha256::digest(query.as_bytes()));
    RetrievalRerankRequest {
        model: "semantic.rerank".into(),
        query: RetrievalTextInput {
            id: "current-query".into(),
            text: query.into(),
            source_revision: query_revision,
        },
        candidates: candidates
            .iter()
            .map(|c| RetrievalTextInput {
                id: c.source.clone(),
                source_revision: format!("sha256:{:x}", Sha256::digest(c.content.as_bytes())),
                text: c.content.chars().take(2400).collect(),
            })
            .collect(),
        top_n: None,
        metadata: BTreeMap::from([
            ("infer.priority".into(), "interactive".into()),
            ("infer.placement".into(), "local_only".into()),
            ("infer.prefer".into(), "local".into()),
            ("infer.offline_required".into(), "true".into()),
            ("infer.fallback".into(), "none".into()),
            ("infer.max_cost_usd".into(), "0".into()),
        ]),
    }
}

fn validate_ranking(
    request: &RetrievalRerankRequest,
    response: RetrievalRerankResponse,
) -> Result<BTreeMap<String, f32>> {
    ensure!(
        response.status == "completed" && response.query_revision == request.query.source_revision,
        "incomplete or wrong-query ranking"
    );
    ensure!(
        response.score_semantics == SCORE_SEMANTICS,
        "unknown ranking scale"
    );
    ensure!(
        response.results.len() == request.candidates.len(),
        "partial ranking"
    );
    let mut scores = BTreeMap::new();
    for result in response.results {
        ensure!(request.candidates.iter().any(|c| c.id == result.candidate_id && c.source_revision == result.source_revision), "wrong ranking identity");
        ensure!(
            result.score.is_finite() && (0.0..=1.0).contains(&result.score),
            "invalid ranking score"
        );
        ensure!(
            scores.insert(result.candidate_id, result.score).is_none(),
            "duplicate ranking identity"
        );
    }
    Ok(scores)
}

fn lexical_scores(query: &str, candidates: &[Candidate]) -> BTreeMap<String, f32> {
    let query = terms(query);
    candidates
        .iter()
        .map(|c| {
            let content = terms(&c.content);
            let overlap = query.intersection(&content).count();
            let score = overlap as f32 / query.len().max(1) as f32;
            (c.source.clone(), score)
        })
        .collect()
}

fn terms(text: &str) -> BTreeSet<String> {
    let lower = text.to_lowercase();
    let mut words = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let chars = lower.chars().collect::<Vec<_>>();
    for pair in chars.windows(2) {
        if pair.iter().all(|c| ('\u{3400}'..='\u{9fff}').contains(c)) {
            let term = pair.iter().collect::<String>();
            if ![
                "现在", "今天", "觉得", "我们", "一个", "这个", "那个", "已经", "可以", "还是",
                "大家", "什么",
            ]
            .contains(&term.as_str())
            {
                words.insert(term);
            }
        }
    }
    words
}

fn admit(
    context: &CompoundContext,
    mut candidates: Vec<Candidate>,
    mut bundle: ContextBundle,
    scores: &BTreeMap<String, f32>,
    lineages: &BTreeMap<String, Lineage>,
    ranker: &str,
) -> ContextBundle {
    let mut audit = context.audit.clone();
    audit.ranker = ranker.into();
    let best = scores.values().copied().fold(0_f32, f32::max);
    let threshold = if ranker == "local_semantic_rerank" {
        // Keep a meaningful relative margin: in real padded batches unrelated
        // history can receive nonzero scores. A permissive tail defeats recall
        // selection, and these scores are not calibrated probabilities.
        0.05_f32.max(best * 0.55)
    } else {
        0.12_f32.max(best * 0.30)
    };
    // Joint relevance is assessed on the same query/scale. Durable evidence is
    // admitted first so a coverage omission can only point to a loaded record.
    candidates.sort_by(|a, b| {
        b.pcp
            .is_some()
            .cmp(&a.pcp.is_some())
            .then_with(|| {
                scores
                    .get(&b.source)
                    .unwrap_or(&0.0)
                    .total_cmp(scores.get(&a.source).unwrap_or(&0.0))
            })
            .then_with(|| b.is_user().cmp(&a.is_user()))
            .then_with(|| a.source.cmp(&b.source))
    });
    let mut remaining = RECALL_CHARS;
    let mut pcp_count = 0;
    let mut local_count = 0;
    let mut loaded_pcp: Vec<&Candidate> = Vec::new();
    for candidate in &candidates {
        let score = scores.get(&candidate.source).copied().unwrap_or_default();
        let lineage = candidate
            .pcp
            .as_ref()
            .and_then(|p| lineages.get(&p.revision_id));
        let mut evidence = RecallCandidateAudit {
            source: candidate.source.clone(),
            score: Some(score),
            source_messages: lineage
                .map(|l| {
                    l.sources
                        .iter()
                        .filter_map(|s| {
                            recall_sources::local_id(
                                s,
                                &context.source_store_id,
                                &candidate.pcp.as_ref().unwrap().namespace,
                            )
                            .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            lineage_complete: lineage.is_some_and(|l| l.complete),
            covered_by: None,
        };
        let covered = loaded_pcp.iter().find(|pcp| {
            covered_by(
                &context.query,
                candidate,
                pcp,
                lineages,
                &context.source_store_id,
            )
        });
        let encoded = candidate.payload.to_string();
        let size = encoded.chars().count();
        let reason = if score < threshold {
            Some("与当前问题的相关性不足；未装入，仍可按来源读取".to_owned())
        } else if let Some(pcp) = covered {
            evidence.covered_by = Some(pcp.source.clone());
            Some(format!(
                "来源链与完整原文匹配，已由 {} 覆盖；不是仅凭相似度判断",
                pcp.source
            ))
        } else if candidate.pcp.is_some() && pcp_count >= MAX_PCP_RECORDS
            || candidate.message.is_some() && local_count >= MAX_LOCAL_RECORDS
        {
            Some("本轮已选更相关的证据；其余候选按需展开".into())
        } else if size > remaining {
            Some("本轮证据预算不足；完整记录保留，按需读取".into())
        } else {
            None
        };
        if let Some(reason) = reason {
            bundle.defer(&candidate.source, &candidate.origin, &reason);
        } else {
            remaining -= size;
            bundle.include(
                &candidate.source,
                &candidate.origin,
                "联合相关性筛选；保留原文与身份限定",
                encoded,
            );
            if candidate.pcp.is_some() {
                pcp_count += 1;
                loaded_pcp.push(candidate);
            } else {
                local_count += 1;
            }
        }
        audit.candidates.push(evidence);
    }
    bundle.recall = Some(audit);
    bundle
}

fn covered_by(
    query: &str,
    raw: &Candidate,
    durable: &Candidate,
    lineages: &BTreeMap<String, Lineage>,
    store: &str,
) -> bool {
    let Some(message) = &raw.message else {
        return false;
    };
    let Some(page) = &durable.pcp else {
        return false;
    };
    let query = query.to_lowercase();
    if [
        "原话",
        "原文",
        "逐字",
        "怎么说",
        "verbatim",
        "exact wording",
        "quote",
    ]
    .iter()
    .any(|s| query.contains(s))
        || message.truncated
        || page.source_projection_truncated
        || page.validity.is_some()
    {
        return false;
    }
    let Some(lineage) = lineages.get(&page.revision_id).filter(|l| l.complete) else {
        return false;
    };
    let digest = format!("sha256:{:x}", Sha256::digest(message.content.as_bytes()));
    let linked = lineage.sources.iter().any(|s| {
        recall_sources::local_id(s, store, &page.namespace) == Some(message.message_id.as_str())
            && s.content_digest
                .as_ref()
                .is_none_or(|expected| expected == &digest)
    });
    // Strict containment only. A paraphrase, partial summary, negation or new
    // correction remains raw evidence until an actual coverage proof exists.
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let raw = normalize(&message.content);
    linked && raw.chars().count() >= 24 && normalize(&durable.content).contains(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{TranscriptSearchCluster, TranscriptSearchResult};

    fn context(query: &str, page: &str, raw: &str) -> CompoundContext {
        CompoundContext {
            query: query.into(), source_store_id: "here".into(), local_available: true, durable_available: true,
            audit: Default::default(),
            durable: Some(serde_json::from_value(json!({
                "scopes":["symbiont-d"],"visibility":"scoped","resultLimit":8,"contextBudgetChars":8000,
                "anchorCount":2,"relatedCount":0,"entries":[
                    {"rank":1,"anchorRank":1,"pageId":"pg_1","revisionId":"rev_1","namespace":"symbiont-d","kind":"concept",
                        "matchedBy":"semantic_vector","matchedProjection":"embedding","semanticScore":0.4,"detail":"payload","sourceProjectionTruncated":false,"content":page},
                    {"rank":2,"anchorRank":2,"pageId":"pg_2","revisionId":"rev_2","namespace":"symbiont-d","kind":"concept",
                        "matchedBy":"semantic_vector","matchedProjection":"embedding","detail":"reference","sourceProjectionTruncated":false}
                ]
            })).unwrap()),
            local: Some(TranscriptSearchResult {
                query: query.into(), recurrence: Default::default(), semantic: Default::default(), truncated: false,
                clusters: vec![TranscriptSearchCluster { score:1.0, source_message_ids:vec!["msg_1".into()], messages:vec![
                    TranscriptSearchMessage { message_id:"msg_1".into(), sequence:1, occurred_at:"2026-09-03T01:00:00Z".into(),
                        role:MemoryRole::User, content:raw.into(), matched:true, truncated:false }
                ] }],
            }),
        }
    }

    fn lineage() -> BTreeMap<String, Lineage> {
        BTreeMap::from([(
            "rev_1".into(),
            Lineage {
                complete: true,
                sources: vec![pcp_core::SourceRef {
                    provider_id: "symbiont:transcript".into(),
                    locator: "store/here/message/msg_1".into(),
                    media_type: None,
                    content_digest: None,
                }],
            },
        )])
    }

    #[test]
    fn joint_admission_rejects_weak_pcp_hit_and_keeps_relevant_user_text_exactly() {
        let context = context(
            "后训练与模型的能力",
            "旧版 Codex 接入配置与订阅额度",
            "后训练可能影响模型能力，但这是猜想。",
        );
        let (candidates, bundle) = candidates(&context);
        let scores = BTreeMap::from([
            ("symbiont.pcp.rev_1".into(), 0.001),
            ("symbiont.transcript.msg_1".into(), 0.8),
        ]);
        let result = admit(
            &context,
            candidates,
            bundle,
            &scores,
            &BTreeMap::new(),
            "local_semantic_rerank",
        );
        assert_eq!(result.fragments.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(&result.fragments[1].value).unwrap()["content"],
            "后训练可能影响模型能力，但这是猜想。"
        );
        assert!(
            result
                .selection
                .iter()
                .any(|s| s.source == "symbiont.pcp.rev_2" && !s.included)
        );
        assert!(
            result
                .selection
                .iter()
                .any(|s| s.source == "symbiont.pcp.rev_1" && s.purpose.contains("相关性不足"))
        );
        let sent = result
            .fragments
            .iter()
            .map(|f| f.value.as_str())
            .collect::<String>();
        assert!(
            !sent.contains("semanticScore")
                && !sent.contains("promotionCandidate")
                && !sent.contains("recurrence")
        );
    }

    #[test]
    fn complete_source_bound_quote_can_cover_raw_but_not_partial_summary_or_new_correction() {
        let raw = "我认为后训练与特定工具环境可能有关，但尚不能把这种关联当成已经验证的因果关系。";
        let context = context("后训练观点", raw, raw);
        let (items, _) = candidates(&context);
        assert!(covered_by(
            &context.query,
            &items[1],
            &items[0],
            &lineage(),
            "here"
        ));
        assert!(!covered_by(
            "当时的原话是什么",
            &items[1],
            &items[0],
            &lineage(),
            "here"
        ));
        let mut partial = self::context("后训练观点", "后训练与工具环境有关。", raw);
        let (items, _) = candidates(&partial);
        assert!(!covered_by(
            "后训练观点",
            &items[1],
            &items[0],
            &lineage(),
            "here"
        ));
        partial.local.as_mut().unwrap().clusters[0].messages[0]
            .content
            .push_str("更正：这不是我的最终判断。");
        let (items, _) = candidates(&partial);
        assert!(!covered_by(
            "后训练观点",
            &items[1],
            &items[0],
            &lineage(),
            "here"
        ));
    }

    #[test]
    fn missing_lineage_wrong_host_digest_or_truncation_never_suppresses_raw() {
        let raw = "用户对该主题的原始完整判断必须保留限定条件，不能仅凭来源关联就删除其原话。";
        let context = context("主题判断", raw, raw);
        let (mut items, _) = candidates(&context);
        assert!(!covered_by(
            "主题判断",
            &items[1],
            &items[0],
            &BTreeMap::new(),
            "here"
        ));
        assert!(!covered_by(
            "主题判断",
            &items[1],
            &items[0],
            &lineage(),
            "other"
        ));
        let mut sources = lineage();
        sources.get_mut("rev_1").unwrap().sources[0].content_digest = Some("sha256:wrong".into());
        assert!(!covered_by(
            "主题判断",
            &items[1],
            &items[0],
            &sources,
            "here"
        ));
        items[1].message.as_mut().unwrap().truncated = true;
        assert!(!covered_by(
            "主题判断",
            &items[1],
            &items[0],
            &lineage(),
            "here"
        ));
    }

    #[test]
    fn raw_coverage_only_points_to_a_page_actually_admitted() {
        let text = "用户明确保留这一判断的限定条件，不能把可能的关联误写成已经证实的结果。";
        let context = context("限定条件", text, text);
        let (items, bundle) = candidates(&context);
        let scores = BTreeMap::from([
            ("symbiont.pcp.rev_1".into(), 0.001),
            ("symbiont.transcript.msg_1".into(), 0.8),
        ]);
        let result = admit(
            &context,
            items,
            bundle,
            &scores,
            &lineage(),
            "local_semantic_rerank",
        );
        assert!(
            result
                .fragments
                .iter()
                .any(|f| f.source == "symbiont.transcript.msg_1")
        );
        assert!(
            result
                .recall
                .unwrap()
                .candidates
                .iter()
                .all(|c| c.covered_by.is_none())
        );
    }

    #[test]
    fn reranker_contract_rejects_partial_wrong_revision_and_unknown_scale() {
        let context = context("query", "page", "raw");
        let (items, _) = candidates(&context);
        let request = rank_request(&context.query, &items);
        assert_eq!(request.metadata["infer.placement"], "local_only");
        assert_eq!(request.metadata["infer.fallback"], "none");
        let response: RetrievalRerankResponse = serde_json::from_value(json!({
            "id":"r","object":"list","created_at":0,"status":"completed","query_revision":request.query.source_revision,
            "results":request.candidates.iter().enumerate().map(|(i,c)| json!({"candidate_id":c.id,"source_revision":c.source_revision,"rank":i+1,"score":0.7})).collect::<Vec<_>>(),
            "score_semantics":SCORE_SEMANTICS,
            "provenance":{"job_id":"r","provider":"local","deployment":"d","model_build":"m","model_revision":"m1","artifact_sha256":"sha","tokenizer_identity":"t","runtime":"mlx","precision":"float16"}
        })).unwrap();
        assert!(validate_ranking(&request, response.clone()).is_ok());
        let mut invalid = response.clone();
        invalid.results.pop();
        assert!(validate_ranking(&request, invalid).is_err());
        let mut invalid = response.clone();
        invalid.results[0].source_revision = "wrong".into();
        assert!(validate_ranking(&request, invalid).is_err());
        let mut invalid = response;
        invalid.score_semantics = "probability".into();
        assert!(validate_ranking(&request, invalid).is_err());
    }

    #[tokio::test]
    #[ignore = "requires the authorized local Infer Runtime; synthetic text only"]
    async fn live_local_rerank_contract() {
        let runtime = crate::infer_runtime::InferRuntimeAccess::open(std::path::PathBuf::from(
            "data/infer-runtime-secrets.toml",
        ))
        .await
        .unwrap();
        let context = context(
            "模型的后训练和特定 Harness 之间有什么关系？",
            "旧版 Codex 接入时如何切换 API Key 配置和订阅额度。",
            "用户认为后训练可能适配特定 Harness 的工作模式，但不把工具环境视为完整训练契约。",
        );
        let (mut items, _) = candidates(&context);
        // Exercise the normal multi-batch envelope, not only a tiny two-item
        // request. Synthetic unrelated history must not outrank user evidence.
        for index in 0..8 {
            items.push(Candidate {
                source: format!("synthetic-history-{index}"),
                content: "这是一段与当前问题无关的历史记录。讨论窗口布局、按钮间距、文档目录，以及如何整理截图和文件名；没有关于模型训练方法的判断。".repeat(12),
                origin: "synthetic".into(),
                payload: Value::Null,
                pcp: None,
                message: None,
            });
        }
        let start = Instant::now();
        let scores = rank(Some(&runtime), &context.query, &items).await.unwrap();
        assert_eq!(scores.len(), items.len());
        assert!(scores["symbiont.transcript.msg_1"] > scores["symbiont.pcp.rev_1"]);
        let (evidence, bundle) = candidates(&context);
        let selected = admit(
            &context,
            evidence,
            bundle,
            &scores,
            &BTreeMap::new(),
            "local_semantic_rerank",
        );
        assert!(
            selected
                .fragments
                .iter()
                .any(|f| f.source == "symbiont.transcript.msg_1")
        );
        assert!(
            !selected
                .fragments
                .iter()
                .any(|f| f.source == "symbiont.pcp.rev_1")
        );
        eprintln!(
            "local rerank verified: {} candidates in {} ms, related={:.4}, unrelated={:.4}",
            items.len(),
            start.elapsed().as_millis(),
            scores["symbiont.transcript.msg_1"],
            scores["symbiont.pcp.rev_1"]
        );
    }
}
