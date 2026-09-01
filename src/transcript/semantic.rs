//! Local semantic index for the authoritative transcript.
//!
//! Infer Runtime owns embedding execution and its exact embedding-space
//! identity. This module only persists derived vectors and joins them back to
//! active user-authored transcript messages. Raw chat remains authoritative;
//! PCP receives durable material only after Reflection judges it worth keeping.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use infer_runtime_client::{
    RetrievalEmbeddingRequest, RetrievalEmbeddingResponse, RetrievalTextInput,
};
use rusqlite::params;
use tokio::{task, time};

use super::{TranscriptStore, open_connection};
use crate::infer_runtime::{InferRuntimeAccess, sdk_error_summary};

const QUERY_MODEL: &str = "semantic.embed_query";
const DOCUMENT_MODEL: &str = "semantic.embed_documents";
const INDEX_BATCH_SIZE: usize = 32;
const MAX_INPUT_CHARS: usize = 8_000;
// The local worker pads every item to the longest input in its batch. Bound
// that padded shape rather than only the item count so one long message does
// not turn an otherwise small backfill batch into 32 full context windows.
const MAX_PADDED_BATCH_CHARS: usize = 8_000;
const MAX_VECTOR_DIMENSIONS: usize = 8_192;
const MIN_SIMILARITY: f64 = 0.55;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub(super) struct SemanticMatch {
    pub(super) message_id: String,
    pub(super) similarity: f64,
}

pub(super) struct SemanticSearchResult {
    pub(super) embedding_space: String,
    pub(super) indexed_user_message_count: usize,
    pub(super) match_count: usize,
    pub(super) matches: Vec<SemanticMatch>,
}

#[derive(Clone, Debug)]
pub(super) struct EmbeddingInput {
    pub(super) id: String,
    pub(super) text: String,
    pub(super) source_revision: String,
}

#[derive(Clone, Debug)]
pub(super) struct EmbeddingVector {
    pub(super) id: String,
    pub(super) source_revision: String,
    pub(super) values: Vec<f32>,
}

#[derive(Clone, Debug)]
pub(super) struct EmbeddingBatch {
    pub(super) space: String,
    pub(super) dimensions: usize,
    pub(super) normalized: bool,
    pub(super) distance_metric: String,
    pub(super) vectors: Vec<EmbeddingVector>,
}

#[async_trait]
pub(super) trait TranscriptEmbedder: Send + Sync {
    async fn embed_query(&self, input: EmbeddingInput) -> Result<EmbeddingBatch>;
    async fn embed_documents(&self, inputs: Vec<EmbeddingInput>) -> Result<EmbeddingBatch>;
}

struct InferTranscriptEmbedder {
    runtime: Arc<InferRuntimeAccess>,
}

#[async_trait]
impl TranscriptEmbedder for InferTranscriptEmbedder {
    async fn embed_query(&self, input: EmbeddingInput) -> Result<EmbeddingBatch> {
        self.embed(QUERY_MODEL, vec![input], true).await
    }

    async fn embed_documents(&self, inputs: Vec<EmbeddingInput>) -> Result<EmbeddingBatch> {
        self.embed(DOCUMENT_MODEL, inputs, false).await
    }
}

impl InferTranscriptEmbedder {
    async fn embed(
        &self,
        model: &str,
        inputs: Vec<EmbeddingInput>,
        query: bool,
    ) -> Result<EmbeddingBatch> {
        ensure!(!inputs.is_empty(), "embedding input batch is empty");
        let request = embedding_request(model, inputs.clone());
        let client = self.runtime.client().await?;
        let response = time::timeout(REQUEST_TIMEOUT, async {
            if query {
                client.sdk().embed_queries(&request).await
            } else {
                client.sdk().embed_documents(&request).await
            }
        })
        .await
        .context("local transcript embedding timed out")?
        .map_err(|error| anyhow::anyhow!(sdk_error_summary(&error)))?;
        let batch = embedding_batch(response)?;
        validate_response_identity(&inputs, &batch)?;
        Ok(batch)
    }
}

pub(super) struct TranscriptSemanticIndex {
    transcript: Arc<TranscriptStore>,
    embedder: Arc<dyn TranscriptEmbedder>,
}

impl TranscriptSemanticIndex {
    pub(super) fn with_infer(
        transcript: Arc<TranscriptStore>,
        runtime: Arc<InferRuntimeAccess>,
    ) -> Self {
        Self::new(transcript, Arc::new(InferTranscriptEmbedder { runtime }))
    }

    pub(super) fn new(
        transcript: Arc<TranscriptStore>,
        embedder: Arc<dyn TranscriptEmbedder>,
    ) -> Self {
        Self {
            transcript,
            embedder,
        }
    }

    pub(super) async fn search(&self, query: &str, limit: usize) -> Result<SemanticSearchResult> {
        let query = query.trim();
        ensure!(!query.is_empty(), "semantic transcript query is empty");
        let query_batch = self
            .embedder
            .embed_query(EmbeddingInput {
                id: "query".to_owned(),
                text: truncate_chars(query, MAX_INPUT_CHARS),
                source_revision: format!("query:{}", stable_query_revision(query)),
            })
            .await?;
        validate_batch(&query_batch, 1)?;
        let query_vector = &query_batch.vectors[0].values;

        // Keep interactive work bounded. The background backfill eventually
        // covers all history; each search also advances the newest missing
        // batch so a freshly appended user message becomes searchable at once.
        let pending = self
            .pending_documents(Some(&query_batch.space), INDEX_BATCH_SIZE)
            .await?;
        if !pending.is_empty() {
            let documents = self.embedder.embed_documents(pending).await?;
            validate_compatible(&query_batch, &documents)?;
            self.persist_batch(&documents).await?;
        }

        let indexed = self.load_vectors(&query_batch.space).await?;
        let indexed_user_message_count = indexed.len();
        let mut matches = indexed
            .into_iter()
            .filter_map(|(message_id, vector)| {
                let similarity = dot_product(query_vector, &vector)?;
                (similarity >= MIN_SIMILARITY).then_some(SemanticMatch {
                    message_id,
                    similarity,
                })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| right.similarity.total_cmp(&left.similarity));
        matches.truncate(limit.clamp(1, 64));
        let match_count = matches.len();
        Ok(SemanticSearchResult {
            embedding_space: query_batch.space,
            indexed_user_message_count,
            match_count,
            matches,
        })
    }

    pub(super) async fn backfill_all(&self) -> Result<usize> {
        // A query embedding is a cheap, payload-free way to discover the
        // Runtime's current exact query/document space. It also makes a model
        // replacement naturally create a parallel, non-corrupting index.
        let probe = self
            .embedder
            .embed_query(EmbeddingInput {
                id: "space-probe".to_owned(),
                text: "聊天记录语义索引".to_owned(),
                source_revision: "symbiont:transcript-space-probe:v1".to_owned(),
            })
            .await?;
        validate_batch(&probe, 1)?;
        let mut indexed = 0;
        loop {
            let pending = self
                .pending_documents(Some(&probe.space), INDEX_BATCH_SIZE)
                .await?;
            if pending.is_empty() {
                break;
            }
            let documents = self.embedder.embed_documents(pending).await?;
            validate_compatible(&probe, &documents)?;
            indexed += documents.vectors.len();
            self.persist_batch(&documents).await?;
            task::yield_now().await;
        }
        Ok(indexed)
    }

    async fn pending_documents(
        &self,
        space: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EmbeddingInput>> {
        let path = self.transcript.path().to_path_buf();
        let space = space.map(str::to_owned);
        task::spawn_blocking(move || -> Result<Vec<EmbeddingInput>> {
            let connection = open_connection(&path)?;
            let mut statement = connection.prepare(
                "SELECT message.message_id, message.content
                 FROM transcript_messages AS message
                 WHERE message.retracted_at IS NULL
                   AND message.role = 'user'
                   AND (?1 IS NULL OR NOT EXISTS (
                       SELECT 1 FROM transcript_message_embeddings AS embedding
                       WHERE embedding.message_id = message.message_id
                         AND embedding.embedding_space = ?1
                   ))
                 ORDER BY message.sequence DESC
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(params![space, limit as i64], |row| {
                let id = row.get::<_, String>(0)?;
                let text = row.get::<_, String>(1)?;
                Ok(EmbeddingInput {
                    source_revision: id.clone(),
                    id,
                    text: truncate_chars(&text, MAX_INPUT_CHARS),
                })
            })?;
            let inputs = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(limit_padded_batch(inputs, MAX_PADDED_BATCH_CHARS))
        })
        .await
        .context("join transcript semantic pending read")?
    }

    async fn persist_batch(&self, batch: &EmbeddingBatch) -> Result<()> {
        let batch = batch.clone();
        let path = self.transcript.path().to_path_buf();
        task::spawn_blocking(move || -> Result<()> {
            let mut connection = open_connection(&path)?;
            let transaction = connection.transaction()?;
            let indexed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            for vector in batch.vectors {
                ensure!(
                    vector.id == vector.source_revision,
                    "document embedding identity does not match transcript revision"
                );
                transaction.execute(
                    "INSERT INTO transcript_message_embeddings(
                         message_id, embedding_space, dimensions, normalized,
                         distance_metric, vector_blob, indexed_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(message_id, embedding_space) DO UPDATE SET
                         dimensions = excluded.dimensions,
                         normalized = excluded.normalized,
                         distance_metric = excluded.distance_metric,
                         vector_blob = excluded.vector_blob,
                         indexed_at = excluded.indexed_at",
                    params![
                        vector.id,
                        batch.space,
                        batch.dimensions as i64,
                        i64::from(batch.normalized),
                        batch.distance_metric,
                        encode_vector(&vector.values),
                        indexed_at,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .context("join transcript semantic persistence")?
    }

    async fn load_vectors(&self, space: &str) -> Result<Vec<(String, Vec<f32>)>> {
        let path = self.transcript.path().to_path_buf();
        let space = space.to_owned();
        task::spawn_blocking(move || -> Result<Vec<(String, Vec<f32>)>> {
            let connection = open_connection(&path)?;
            let mut statement = connection.prepare(
                "SELECT message.message_id, embedding.vector_blob, embedding.dimensions
                 FROM transcript_message_embeddings AS embedding
                 JOIN transcript_messages AS message ON message.message_id = embedding.message_id
                 WHERE embedding.embedding_space = ?1
                   AND embedding.normalized = 1
                   AND embedding.distance_metric = 'cosine'
                   AND message.retracted_at IS NULL
                   AND message.role = 'user'",
            )?;
            let rows = statement.query_map([space], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            let mut vectors = Vec::new();
            for row in rows {
                let (message_id, blob, dimensions) = row?;
                let Ok(dimensions) = usize::try_from(dimensions) else {
                    continue;
                };
                if let Some(vector) = decode_vector(&blob, dimensions) {
                    vectors.push((message_id, vector));
                }
            }
            Ok(vectors)
        })
        .await
        .context("join transcript semantic vector read")?
    }
}

fn embedding_request(model: &str, inputs: Vec<EmbeddingInput>) -> RetrievalEmbeddingRequest {
    RetrievalEmbeddingRequest {
        model: model.to_owned(),
        inputs: inputs
            .into_iter()
            .map(|input| RetrievalTextInput {
                id: input.id,
                text: input.text,
                source_revision: input.source_revision,
            })
            .collect(),
        metadata: BTreeMap::from([
            ("infer.priority".to_owned(), "background".to_owned()),
            ("infer.placement".to_owned(), "local_only".to_owned()),
            ("infer.prefer".to_owned(), "local".to_owned()),
            ("infer.offline_required".to_owned(), "true".to_owned()),
            ("infer.fallback".to_owned(), "none".to_owned()),
            ("infer.max_cost_usd".to_owned(), "0".to_owned()),
        ]),
    }
}

fn embedding_batch(response: RetrievalEmbeddingResponse) -> Result<EmbeddingBatch> {
    ensure!(
        response.status == "completed",
        "embedding response is incomplete"
    );
    let first = response
        .data
        .first()
        .context("embedding response omitted vectors")?;
    for item in &response.data {
        ensure!(
            item.embedding.space == first.embedding.space
                && item.embedding.dimensions == first.embedding.dimensions
                && item.embedding.normalized == first.embedding.normalized
                && item.embedding.distance_metric == first.embedding.distance_metric,
            "embedding response mixed incompatible vector contracts"
        );
    }
    let batch = EmbeddingBatch {
        space: first.embedding.space.clone(),
        dimensions: first.embedding.dimensions,
        normalized: first.embedding.normalized,
        distance_metric: first.embedding.distance_metric.clone(),
        vectors: response
            .data
            .into_iter()
            .map(|item| EmbeddingVector {
                id: item.id,
                source_revision: item.source_revision,
                values: item.embedding.values,
            })
            .collect(),
    };
    validate_batch(&batch, batch.vectors.len())?;
    Ok(batch)
}

fn validate_response_identity(inputs: &[EmbeddingInput], batch: &EmbeddingBatch) -> Result<()> {
    ensure!(
        batch.vectors.len() == inputs.len(),
        "embedding response count does not match request"
    );
    for input in inputs {
        ensure!(
            batch
                .vectors
                .iter()
                .filter(|vector| {
                    vector.id == input.id && vector.source_revision == input.source_revision
                })
                .count()
                == 1,
            "embedding response identity does not match request"
        );
    }
    Ok(())
}

fn validate_batch(batch: &EmbeddingBatch, expected: usize) -> Result<()> {
    ensure!(!batch.space.trim().is_empty(), "embedding space is empty");
    ensure!(
        (1..=MAX_VECTOR_DIMENSIONS).contains(&batch.dimensions),
        "embedding dimensions are invalid"
    );
    ensure!(batch.normalized, "transcript embeddings must be normalized");
    ensure!(
        batch.distance_metric == "cosine",
        "transcript embedding distance metric must be cosine"
    );
    ensure!(
        batch.vectors.len() == expected,
        "embedding response count does not match request"
    );
    for vector in &batch.vectors {
        ensure!(
            vector.values.len() == batch.dimensions,
            "embedding dimensions do not match vector length"
        );
        ensure!(
            vector.values.iter().all(|value| value.is_finite()),
            "embedding vector contains non-finite values"
        );
    }
    Ok(())
}

fn validate_compatible(query: &EmbeddingBatch, documents: &EmbeddingBatch) -> Result<()> {
    validate_batch(documents, documents.vectors.len())?;
    ensure!(
        query.space == documents.space,
        "embedding spaces do not match"
    );
    ensure!(
        query.dimensions == documents.dimensions,
        "embedding dimensions do not match"
    );
    ensure!(
        query.normalized == documents.normalized
            && query.distance_metric == documents.distance_metric,
        "embedding contracts do not match"
    );
    Ok(())
}

fn encode_vector(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn decode_vector(blob: &[u8], dimensions: usize) -> Option<Vec<f32>> {
    if dimensions == 0 || dimensions > MAX_VECTOR_DIMENSIONS || blob.len() != dimensions * 4 {
        return None;
    }
    let values = blob
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}

fn dot_product(left: &[f32], right: &[f32]) -> Option<f64> {
    (left.len() == right.len()).then(|| {
        left.iter()
            .zip(right)
            .map(|(left, right)| f64::from(*left) * f64::from(*right))
            .sum()
    })
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn limit_padded_batch(inputs: Vec<EmbeddingInput>, max_padded_chars: usize) -> Vec<EmbeddingInput> {
    let mut selected = Vec::with_capacity(inputs.len());
    let mut longest_chars = 0usize;
    for input in inputs {
        let input_chars = input.text.chars().count().max(1);
        let next_longest = longest_chars.max(input_chars);
        let padded_chars = next_longest.saturating_mul(selected.len() + 1);
        if !selected.is_empty() && padded_chars > max_padded_chars {
            break;
        }
        selected.push(input);
        longest_chars = next_longest;
    }
    selected
}

fn stable_query_revision(query: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(query.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_strictly_local_and_offline() {
        let request = embedding_request(
            QUERY_MODEL,
            vec![EmbeddingInput {
                id: "query".to_owned(),
                text: "hello".to_owned(),
                source_revision: "query:1".to_owned(),
            }],
        );
        assert_eq!(request.model, QUERY_MODEL);
        assert_eq!(request.metadata["infer.placement"], "local_only");
        assert_eq!(request.metadata["infer.offline_required"], "true");
        assert_eq!(request.metadata["infer.fallback"], "none");
        assert_eq!(request.metadata["infer.max_cost_usd"], "0");
    }

    #[test]
    fn vector_blob_round_trips() {
        let values = vec![0.25, -0.5, 0.75];
        assert_eq!(decode_vector(&encode_vector(&values), 3), Some(values));
        assert!(decode_vector(&[0; 3], 3).is_none());
    }

    #[test]
    fn padded_batch_budget_is_not_amplified_by_one_long_message() {
        let inputs = vec![
            EmbeddingInput {
                id: "long".to_owned(),
                text: "长".repeat(7_789),
                source_revision: "long".to_owned(),
            },
            EmbeddingInput {
                id: "short".to_owned(),
                text: "短消息".to_owned(),
                source_revision: "short".to_owned(),
            },
        ];
        let selected = limit_padded_batch(inputs, MAX_PADDED_BATCH_CHARS);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "long");
    }

    #[test]
    fn padded_batch_budget_keeps_small_messages_batched() {
        let inputs = (0..32)
            .map(|index| EmbeddingInput {
                id: index.to_string(),
                text: "短消息".repeat(10),
                source_revision: index.to_string(),
            })
            .collect();
        assert_eq!(limit_padded_batch(inputs, MAX_PADDED_BATCH_CHARS).len(), 32);
    }
}
