//! Candidate-specific historical retrieval for duplicate review. Similarity is
//! never a deletion verdict. Exact-space local embeddings augment lexical/source
//! retrieval; runtime failures leave candidates eligible for normal review.

use crate::{
    infer_runtime::{InferRuntimeAccess, sdk_error_summary},
    sensing::{SensingCandidate, SensingDeduplicationReference},
};
use anyhow::{Context, Result, ensure};
use infer_runtime_client::{
    RetrievalEmbeddingRequest, RetrievalEmbeddingResponse, RetrievalTextInput,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio::{
    sync::{Mutex, watch},
    time::{Duration, timeout},
};

const MAX_TEXT_CHARS: usize = 1800;
const PADDED_BATCH_CHARS: usize = 8000;

#[derive(Default)]
struct Cache {
    contract: Option<(String, usize)>,
    vectors: HashMap<String, Vec<f32>>,
}

#[derive(Default)]
pub(super) struct SensingSimilarity {
    cache: Mutex<Cache>,
}

impl SensingSimilarity {
    pub(super) async fn retrieve(
        &self,
        runtime: &InferRuntimeAccess,
        candidates: &[SensingCandidate],
        references: &[SensingDeduplicationReference],
        mut input: watch::Receiver<u64>,
    ) -> Vec<Vec<SensingDeduplicationReference>> {
        let candidate_texts = candidates
            .iter()
            .map(|c| {
                bounded(&format!(
                    "{}\n{}",
                    c.title,
                    if c.received_text.is_empty() {
                        &c.summary
                    } else {
                        &c.received_text
                    }
                ))
            })
            .collect::<Vec<_>>();
        let mut all = references.to_vec();
        all.extend(candidates.iter().map(|c| SensingDeduplicationReference {
            reference_id: format!("candidate:{}", c.id),
            fingerprint: c.fingerprint.clone(),
            actor_name: c.actor.name.clone(),
            title: c.title.clone(),
            excerpt: bounded(if c.received_text.is_empty() {
                &c.summary
            } else {
                &c.received_text
            }),
            source_urls: c.sources.iter().map(|s| s.url.clone()).collect(),
            event_at: c.event_at.clone(),
            source_document_at: c.source_document_at.clone(),
            observed_at: c.observed_at.clone(),
        }));
        let texts = all
            .iter()
            .map(|r| bounded(&format!("{}\n{}", r.title, r.excerpt)))
            .collect::<Vec<_>>();
        let mut cache = self.cache.lock().await;
        let active = texts
            .iter()
            .chain(&candidate_texts)
            .map(|t| key(t))
            .collect::<HashSet<_>>();
        cache.vectors.retain(|id, _| active.contains(id));
        // Probe current space with fresh candidate vectors on every pass. Cached
        // vectors are never compared across model/deployment-space changes.
        let result = tokio::select! {
            _ = input.changed() => Err(anyhow::anyhow!("new user input")),
            result = timeout(Duration::from_secs(60), warm(runtime, &mut cache, &candidate_texts, &texts)) => result.context("historical similarity budget elapsed").and_then(|r|r),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "historical duplicate retrieval using lexical/source fallback where vectors are unavailable");
        }
        candidate_texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let permitted = references.len() + index;
                shortlist(
                    text,
                    &candidates[index],
                    &all[..permitted],
                    &texts[..permitted],
                    &cache,
                )
            })
            .collect()
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_TEXT_CHARS).collect()
}
fn key(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn batch_len(texts: &[String]) -> usize {
    let mut longest = 0;
    let mut count = 0;
    for text in texts {
        longest = longest.max(text.chars().count());
        if longest * (count + 1) > PADDED_BATCH_CHARS {
            break;
        }
        count += 1;
    }
    count.max(1).min(texts.len())
}

async fn warm(
    runtime: &InferRuntimeAccess,
    cache: &mut Cache,
    candidates: &[String],
    texts: &[String],
) -> Result<()> {
    let mut probe = candidates.to_vec();
    probe.sort();
    probe.dedup();
    let mut offset = 0;
    while offset < probe.len() {
        let end = offset + batch_len(&probe[offset..]);
        embed(runtime, cache, &probe[offset..end]).await?;
        offset = end;
    }
    let mut missing = texts
        .iter()
        .filter(|t| !cache.vectors.contains_key(&key(t)))
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();
    missing.dedup();
    let mut offset = 0;
    while offset < missing.len() {
        let end = offset + batch_len(&missing[offset..]);
        embed(runtime, cache, &missing[offset..end]).await?;
        offset = end;
    }
    Ok(())
}

async fn embed(runtime: &InferRuntimeAccess, cache: &mut Cache, texts: &[String]) -> Result<()> {
    let request = RetrievalEmbeddingRequest {
        model: "semantic.embed_documents".into(),
        inputs: texts
            .iter()
            .map(|text| RetrievalTextInput {
                id: key(text),
                source_revision: key(text),
                text: text.clone(),
            })
            .collect(),
        metadata: BTreeMap::from([
            ("infer.priority".into(), "background".into()),
            ("infer.placement".into(), "local_only".into()),
            ("infer.prefer".into(), "local".into()),
            ("infer.offline_required".into(), "true".into()),
            ("infer.fallback".into(), "none".into()),
            ("infer.max_cost_usd".into(), "0".into()),
        ]),
    };
    let client = runtime.client().await?;
    let response = client
        .sdk()
        .embed_documents(&request)
        .await
        .map_err(|e| anyhow::anyhow!(sdk_error_summary(&e)))?;
    accept(cache, &request, response)
}

fn accept(
    cache: &mut Cache,
    request: &RetrievalEmbeddingRequest,
    response: RetrievalEmbeddingResponse,
) -> Result<()> {
    ensure!(
        response.status == "completed" && response.data.len() == request.inputs.len(),
        "incomplete historical embeddings"
    );
    let first = &response
        .data
        .first()
        .context("missing embeddings")?
        .embedding;
    let contract = (first.space.clone(), first.dimensions);
    ensure!(
        !contract.0.is_empty() && contract.1 > 0 && contract.1 <= 8192,
        "invalid embedding space"
    );
    let mut expected = request
        .inputs
        .iter()
        .map(|i| (i.id.as_str(), i.source_revision.as_str()))
        .collect::<HashMap<_, _>>();
    for item in &response.data {
        ensure!(
            expected.remove(item.id.as_str()) == Some(item.source_revision.as_str()),
            "wrong embedding identity"
        );
        let vector = &item.embedding;
        ensure!(
            vector.space == contract.0
                && vector.dimensions == contract.1
                && vector.normalized
                && vector.distance_metric == "cosine",
            "incompatible embedding contracts"
        );
        ensure!(
            vector.values.len() == contract.1
                && vector.values.iter().all(|v| v.is_finite())
                && vector.values.iter().any(|v| *v != 0.0),
            "invalid embedding vector"
        );
    }
    if cache.contract.as_ref() != Some(&contract) {
        cache.vectors.clear();
        cache.contract = Some(contract);
    }
    for item in response.data {
        cache.vectors.insert(item.id, item.embedding.values);
    }
    Ok(())
}

fn tokens(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    let mut tokens = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 1)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let chars = lower.chars().collect::<Vec<_>>();
    tokens.extend(
        chars
            .windows(2)
            .filter(|pair| pair.iter().all(|c| ('\u{3400}'..='\u{9fff}').contains(c)))
            .map(|pair| pair.iter().collect()),
    );
    tokens
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return -1.0;
    }
    let dot = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum::<f64>();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    dot / (norm(a) * norm(b)).max(f64::EPSILON)
}

fn shortlist(
    text: &str,
    candidate: &SensingCandidate,
    references: &[SensingDeduplicationReference],
    texts: &[String],
    cache: &Cache,
) -> Vec<SensingDeduplicationReference> {
    let query = tokens(text);
    let query_vector = cache.vectors.get(&key(text));
    let mut lexical = Vec::new();
    let mut semantic = Vec::new();
    for (index, (reference, text)) in references.iter().zip(texts).enumerate() {
        let words = tokens(text);
        let score = query.intersection(&words).count() as f64
            / (query.len().min(words.len()).max(1) as f64);
        let shared_url = reference
            .source_urls
            .iter()
            .any(|url| candidate.sources.iter().any(|s| &s.url == url));
        lexical.push((index, score + if shared_url { 1.0 } else { 0.0 }));
        if let (Some(a), Some(b)) = (query_vector, cache.vectors.get(&key(text))) {
            semantic.push((index, cosine(a, b)));
        }
    }
    lexical.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    semantic.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut selected = Vec::new();
    for index in lexical
        .iter()
        .take(4)
        .map(|r| r.0)
        .chain(semantic.iter().take(6).map(|r| r.0))
        .chain(0..references.len().min(2))
    {
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected
        .into_iter()
        .map(|index| references[index].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensing::{InputRoleSnapshot, SensingSourceClass};

    #[tokio::test]
    #[ignore = "requires the authorized local Infer Runtime; no persistent writes"]
    async fn live_local_embedding_contract() {
        let runtime =
            InferRuntimeAccess::open(std::path::PathBuf::from("data/infer-runtime-secrets.toml"))
                .await
                .unwrap();
        let mut cache = Cache::default();
        let texts = vec![
            "SWE-bench Pro：同一版本模型的评测分数未改变。".to_owned(),
            "SWE-bench Pro：还是之前模型版本的相同分数，没有新评测。".to_owned(),
            "新的列车时刻表公布了发车时间。".to_owned(),
        ];
        timeout(Duration::from_secs(60), embed(&runtime, &mut cache, &texts))
            .await
            .unwrap()
            .unwrap();
        let a = &cache.vectors[&key(&texts[0])];
        let same = cosine(a, &cache.vectors[&key(&texts[1])]);
        let other = cosine(a, &cache.vectors[&key(&texts[2])]);
        assert!(same > other);
        println!(
            "local embeddings verified: dimensions={}, related={same:.3}, unrelated={other:.3}",
            a.len()
        );
    }
    #[test]
    fn retrieval_can_reach_old_semantic_and_lexical_matches_beyond_recent_24() {
        let candidate = SensingCandidate {
            id: "new".into(),
            title: "SWE-bench Pro".into(),
            summary: "same score".into(),
            received_text: "same score".into(),
            proposed_input: String::new(),
            actor: InputRoleSnapshot::mailbox("test"),
            sources: vec![],
            source_class: SensingSourceClass::Research,
            possible_connection: None,
            event_at: None,
            source_document_at: None,
            observed_at: String::new(),
            expires_at: String::new(),
            fingerprint: String::new(),
        };
        let references = (0..60)
            .map(|i| SensingDeduplicationReference {
                reference_id: i.to_string(),
                title: format!("unrelated {i}"),
                excerpt: String::new(),
                source_urls: vec![],
                actor_name: String::new(),
                fingerprint: String::new(),
                event_at: None,
                source_document_at: None,
                observed_at: String::new(),
            })
            .collect::<Vec<_>>();
        let mut texts = references
            .iter()
            .map(|r| r.title.clone())
            .collect::<Vec<_>>();
        texts[58] = "SWE-bench Pro same score".into();
        let mut cache = Cache::default();
        cache.vectors.insert(key("SWE-bench Pro"), vec![1.0, 0.0]);
        cache.vectors.insert(key(&texts[59]), vec![1.0, 0.0]);
        let matches = shortlist("SWE-bench Pro", &candidate, &references, &texts, &cache);
        assert!(matches.iter().any(|r| r.reference_id == "58"));
        assert!(matches.iter().any(|r| r.reference_id == "59"));
        assert!(matches.len() <= 12);
    }
    #[test]
    fn padding_not_item_count_limits_embedding_batches() {
        assert_eq!(batch_len(&vec!["x".repeat(1800); 20]), 4);
        assert_eq!(batch_len(&vec!["x".repeat(100); 20]), 20);
    }
    #[test]
    fn fallback_matches_chinese_fragments_and_english_benchmarks() {
        assert!(tokens("SWE-bench Pro 主张范围过强").contains("swe"));
        assert!(tokens("主张范围过强").contains("范围"));
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), 1.0);
    }
}
