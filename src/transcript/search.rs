//! Bounded lexical recall over the authoritative local transcript.
//!
//! This owner is deliberately local and dependency-light. PCP remains the
//! durable cross-platform memory, while this index lets Symbiont recover raw
//! chat evidence and notice user subjects that become important only after
//! recurring across time.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use anyhow::{Result, bail};
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use super::{TranscriptStore, open_connection};
use crate::memory::MemoryRole;

const MAX_QUERY_CHARS: usize = 512;
const MAX_TERMS: usize = 96;
const MAX_TERM_CHARS: usize = 64;
const MAX_INDEX_CJK_TERMS: usize = 20_000;
const MAX_CLUSTERS: usize = 12;
const MAX_MESSAGES: usize = 64;
const MAX_OUTPUT_CHARS: usize = 32_000;
const MAX_CONTEXT_PER_SIDE: usize = 4;
const MAX_MESSAGE_CHARS: usize = 4_000;
const MAX_CANDIDATES: usize = 256;
const DEFAULT_EPISODE_GAP_HOURS: i64 = 6;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSearchOptions {
    /// Maximum number of independently relevant windows returned.
    pub max_clusters: usize,
    /// Global cap across every returned window.
    pub max_messages: usize,
    /// Global Unicode-character cap across every returned message.
    pub max_chars: usize,
    /// Raw visible messages admitted before a matching message.
    pub context_before: usize,
    /// Raw visible messages admitted after a matching message.
    pub context_after: usize,
    /// A larger gap starts a new inferred conversation episode.
    pub episode_gap_hours: i64,
}

impl Default for TranscriptSearchOptions {
    fn default() -> Self {
        Self {
            max_clusters: 6,
            max_messages: 32,
            max_chars: 12_000,
            context_before: 2,
            context_after: 2,
            episode_gap_hours: DEFAULT_EPISODE_GAP_HOURS,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSearchResult {
    pub query: String,
    pub clusters: Vec<TranscriptSearchCluster>,
    pub recurrence: TranscriptRecurrenceEvidence,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSearchCluster {
    pub score: f64,
    pub source_message_ids: Vec<String>,
    pub messages: Vec<TranscriptSearchMessage>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSearchMessage {
    pub message_id: String,
    pub sequence: i64,
    pub occurred_at: String,
    pub role: MemoryRole,
    /// Exact stored content unless `truncated` is true.
    pub content: String,
    pub matched: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSourceOptions {
    pub context_before: usize,
    pub context_after: usize,
    pub target_max_chars: usize,
    pub neighbor_max_chars: usize,
    pub max_chars: usize,
    pub episode_gap_hours: i64,
}

impl Default for TranscriptSourceOptions {
    fn default() -> Self {
        Self {
            context_before: 2,
            context_after: 2,
            target_max_chars: 6_000,
            neighbor_max_chars: 1_500,
            max_chars: 12_000,
            episode_gap_hours: DEFAULT_EPISODE_GAP_HOURS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSourceStatus {
    Active,
    Retracted,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSourceResolution {
    pub source_message_id: String,
    pub status: TranscriptSourceStatus,
    /// Chronological same-episode context. The target has `matched=true`.
    pub messages: Vec<TranscriptSearchMessage>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptRecurrenceEvidence {
    /// Only user-authored matches count toward delayed-promotion evidence.
    pub user_match_count: usize,
    pub distinct_day_count: usize,
    /// Session-like episodes inferred from the configured temporal gap.
    pub distinct_episode_count: usize,
    pub episode_gap_hours: i64,
    pub first_user_mention_at: Option<String>,
    pub last_user_mention_at: Option<String>,
    pub source_message_ids: Vec<String>,
    pub repeated_across_time: bool,
}

/// Stable composition boundary for background recurrence checks and raw recall.
#[derive(Clone)]
pub struct TranscriptRecall {
    transcript: Arc<TranscriptStore>,
}

impl TranscriptRecall {
    pub fn new(transcript: Arc<TranscriptStore>) -> Self {
        Self { transcript }
    }

    pub async fn search(
        &self,
        query: &str,
        options: TranscriptSearchOptions,
    ) -> Result<TranscriptSearchResult> {
        self.transcript.search(query, options).await
    }

    pub async fn recurrence_evidence(
        &self,
        query: &str,
        options: TranscriptSearchOptions,
    ) -> Result<TranscriptRecurrenceEvidence> {
        Ok(self.search(query, options).await?.recurrence)
    }

    pub async fn resolve_source(
        &self,
        message_id: &str,
        options: TranscriptSourceOptions,
    ) -> Result<TranscriptSourceResolution> {
        self.transcript.resolve_source(message_id, options).await
    }
}

#[derive(Clone, Debug)]
struct BoundedOptions {
    max_clusters: usize,
    max_messages: usize,
    max_chars: usize,
    context_before: usize,
    context_after: usize,
    episode_gap_hours: i64,
    candidate_limit: usize,
}

impl From<TranscriptSearchOptions> for BoundedOptions {
    fn from(options: TranscriptSearchOptions) -> Self {
        let max_clusters = options.max_clusters.clamp(1, MAX_CLUSTERS);
        let max_messages = options.max_messages.clamp(1, MAX_MESSAGES);
        Self {
            max_clusters,
            max_messages,
            max_chars: options.max_chars.clamp(1, MAX_OUTPUT_CHARS),
            context_before: options.context_before.min(MAX_CONTEXT_PER_SIDE),
            context_after: options.context_after.min(MAX_CONTEXT_PER_SIDE),
            episode_gap_hours: options.episode_gap_hours.clamp(1, 24 * 7),
            candidate_limit: (max_clusters * 24).max(64).min(MAX_CANDIDATES),
        }
    }
}

#[derive(Clone, Debug)]
struct StoredMessage {
    message_id: String,
    sequence: i64,
    occurred_at: String,
    role: MemoryRole,
    content: String,
}

#[derive(Clone, Debug)]
struct Candidate {
    message: StoredMessage,
    score: f64,
}

#[derive(Debug)]
struct BuiltCluster {
    score: f64,
    anchor_ids: HashSet<String>,
    messages: BTreeMap<i64, StoredMessage>,
}

pub(super) fn initialize_index(connection: &Connection) -> Result<()> {
    let has_cjk_terms = {
        let mut statement = connection.prepare("PRAGMA table_info(transcript_messages_fts)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|column| column == "cjk_terms")
    };
    let index_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'transcript_messages_fts'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if index_exists && !has_cjk_terms {
        // This is a derived index. Rebuilding is safer than carrying a legacy
        // tokenizer layout that cannot recover topics from long CJK sentences.
        connection.execute("DROP TABLE transcript_messages_fts", [])?;
    }
    connection.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS transcript_messages_fts
         USING fts5(
             message_id UNINDEXED,
             content,
             cjk_terms,
             tokenize='unicode61 remove_diacritics 2'
         );",
    )?;
    let missing = {
        let mut statement = connection.prepare(
            "SELECT message_id, content FROM transcript_messages AS message
             WHERE NOT EXISTS (
                 SELECT 1 FROM transcript_messages_fts AS indexed
                 WHERE indexed.message_id = message.message_id
             )",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (message_id, content) in missing {
        index_message(connection, &message_id, &content)?;
    }
    Ok(())
}

pub(super) fn index_message(
    connection: &Connection,
    message_id: &str,
    content: &str,
) -> Result<()> {
    connection.execute(
        "DELETE FROM transcript_messages_fts WHERE message_id = ?1",
        [message_id],
    )?;
    connection.execute(
        "INSERT INTO transcript_messages_fts(message_id, content, cjk_terms)
         VALUES (?1, ?2, ?3)",
        params![message_id, content, cjk_index_text(content)],
    )?;
    Ok(())
}

pub(super) fn search_transcript(
    path: &Path,
    query: &str,
    options: TranscriptSearchOptions,
) -> Result<TranscriptSearchResult> {
    let query = normalize_query(query)?;
    let terms = lexical_terms(&query);
    let options = BoundedOptions::from(options);
    let connection = open_connection(path)?;
    let candidates = search_candidates(&connection, &query, &terms, options.candidate_limit)?;
    let recurrence = recurrence_evidence(&candidates, options.episode_gap_hours);
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.message.message_id.clone())
        .collect::<HashSet<_>>();
    let mut clusters = build_clusters(&connection, &candidates, &options)?;
    clusters.sort_by(|left, right| right.score.total_cmp(&left.score));
    let (clusters, truncated) = materialize_clusters(
        clusters,
        &candidate_ids,
        options.max_clusters,
        options.max_messages,
        options.max_chars,
    );
    Ok(TranscriptSearchResult {
        query,
        clusters,
        recurrence,
        truncated: truncated || candidates.len() >= options.candidate_limit,
    })
}

pub(super) fn resolve_source(
    path: &Path,
    message_id: &str,
    options: TranscriptSourceOptions,
) -> Result<TranscriptSourceResolution> {
    let message_id = message_id.trim();
    if message_id.is_empty() || message_id.chars().count() > 256 {
        bail!("invalid transcript source message id");
    }
    let connection = open_connection(path)?;
    let target = connection
        .query_row(
            "SELECT message_id, sequence, occurred_at, role, content, retracted_at
             FROM transcript_messages WHERE message_id = ?1",
            [message_id],
            |row| {
                let message = stored_message(row)?;
                let retracted_at = row.get::<_, Option<String>>(5)?;
                Ok((message, retracted_at.is_some()))
            },
        )
        .optional()?;
    let Some((target, retracted)) = target else {
        return Ok(TranscriptSourceResolution {
            source_message_id: message_id.to_owned(),
            status: TranscriptSourceStatus::Unavailable,
            messages: Vec::new(),
            truncated: false,
        });
    };
    if retracted {
        return Ok(TranscriptSourceResolution {
            source_message_id: message_id.to_owned(),
            status: TranscriptSourceStatus::Retracted,
            messages: Vec::new(),
            truncated: false,
        });
    }

    let context_before = options.context_before.min(MAX_CONTEXT_PER_SIDE);
    let context_after = options.context_after.min(MAX_CONTEXT_PER_SIDE);
    let target_max_chars = options.target_max_chars.clamp(1, 12_000);
    let neighbor_max_chars = options.neighbor_max_chars.clamp(1, MAX_MESSAGE_CHARS);
    let mut remaining_chars = options.max_chars.clamp(1, MAX_OUTPUT_CHARS);
    let episode_gap_hours = options.episode_gap_hours.clamp(1, 24 * 7);
    let window = context_window(
        &connection,
        target.sequence,
        context_before,
        context_after,
        episode_gap_hours,
    )?;
    let mut truncated = context_before != options.context_before
        || context_after != options.context_after
        || target_max_chars != options.target_max_chars
        || neighbor_max_chars != options.neighbor_max_chars
        || remaining_chars != options.max_chars
        || episode_gap_hours != options.episode_gap_hours;
    let mut selected = Vec::<(StoredMessage, String, bool)>::new();

    // Reserve the target first so large neighbors cannot consume its evidence budget.
    let target_allowed = remaining_chars.min(target_max_chars);
    let (target_content, target_clipped) = truncate_chars(&target.content, target_allowed);
    remaining_chars = remaining_chars.saturating_sub(target_content.chars().count());
    truncated |= target_clipped;
    selected.push((target.clone(), target_content, target_clipped));

    let Some(target_index) = window
        .iter()
        .position(|message| message.message_id == target.message_id)
    else {
        bail!("active transcript source disappeared while resolving context");
    };
    for distance in 1..window.len() {
        for index in [
            target_index.checked_sub(distance),
            target_index.checked_add(distance),
        ]
        .into_iter()
        .flatten()
        .filter(|index| *index < window.len())
        {
            if remaining_chars == 0 {
                truncated = true;
                break;
            }
            let message = &window[index];
            let allowed = remaining_chars.min(neighbor_max_chars);
            let (content, clipped) = truncate_chars(&message.content, allowed);
            remaining_chars = remaining_chars.saturating_sub(content.chars().count());
            truncated |= clipped;
            selected.push((message.clone(), content, clipped));
        }
        if remaining_chars == 0 {
            break;
        }
    }
    selected.sort_by_key(|(message, _, _)| message.sequence);
    let messages = selected
        .into_iter()
        .map(|(message, content, clipped)| TranscriptSearchMessage {
            matched: message.message_id == target.message_id,
            message_id: message.message_id,
            sequence: message.sequence,
            occurred_at: message.occurred_at,
            role: message.role,
            content,
            truncated: clipped,
        })
        .collect();
    Ok(TranscriptSourceResolution {
        source_message_id: message_id.to_owned(),
        status: TranscriptSourceStatus::Active,
        messages,
        truncated,
    })
}

fn normalize_query(query: &str) -> Result<String> {
    let query = query.trim();
    if query.is_empty() {
        bail!("transcript search query is empty");
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        bail!("transcript search query exceeds {MAX_QUERY_CHARS} characters");
    }
    Ok(query.to_owned())
}

fn lexical_terms(query: &str) -> Vec<String> {
    let mut ascii_terms = Vec::new();
    let mut cjk_terms = Vec::new();
    let mut ascii = String::new();
    let mut cjk = String::new();
    let flush_ascii = |terms: &mut Vec<String>, current: &mut String| {
        if !current.is_empty() {
            let term = current.chars().take(MAX_TERM_CHARS).collect::<String>();
            if !terms.contains(&term) {
                terms.push(term);
            }
            current.clear();
        }
    };
    let flush_cjk = |terms: &mut Vec<String>, current: &mut String| {
        if !current.is_empty() {
            add_cjk_ngrams(terms, current, usize::MAX);
            current.clear();
        }
    };
    for character in query.chars() {
        if is_cjk(character) {
            flush_ascii(&mut ascii_terms, &mut ascii);
            cjk.push(character);
        } else if character.is_alphanumeric() {
            flush_cjk(&mut cjk_terms, &mut cjk);
            ascii.extend(character.to_lowercase());
        } else {
            flush_ascii(&mut ascii_terms, &mut ascii);
            flush_cjk(&mut cjk_terms, &mut cjk);
        }
    }
    flush_ascii(&mut ascii_terms, &mut ascii);
    flush_cjk(&mut cjk_terms, &mut cjk);

    let mut terms = ascii_terms;
    let remaining = MAX_TERMS.saturating_sub(terms.len());
    terms.extend(sample_evenly(cjk_terms, remaining));
    let mut seen = HashSet::new();
    terms.retain(|term| seen.insert(term.clone()));
    terms.truncate(MAX_TERMS);
    terms
}

fn cjk_index_text(content: &str) -> String {
    let mut terms = Vec::new();
    let mut run = String::new();
    for character in content.chars() {
        if is_cjk(character) {
            run.push(character);
        } else if !run.is_empty() {
            add_cjk_ngrams(&mut terms, &run, MAX_INDEX_CJK_TERMS);
            run.clear();
        }
        if terms.len() >= MAX_INDEX_CJK_TERMS {
            break;
        }
    }
    if terms.len() < MAX_INDEX_CJK_TERMS && !run.is_empty() {
        add_cjk_ngrams(&mut terms, &run, MAX_INDEX_CJK_TERMS);
    }
    terms.join(" ")
}

fn add_cjk_ngrams(terms: &mut Vec<String>, run: &str, limit: usize) {
    let characters = run.chars().collect::<Vec<_>>();
    if characters.len() < 2 {
        if let Some(character) = characters.first()
            && terms.len() < limit
        {
            terms.push(character.to_string());
        }
        return;
    }
    for width in [3_usize, 2] {
        if characters.len() < width {
            continue;
        }
        for window in characters.windows(width) {
            if terms.len() >= limit {
                return;
            }
            let term = window.iter().collect::<String>();
            terms.push(term);
        }
    }
}

fn sample_evenly<T>(values: Vec<T>, limit: usize) -> Vec<T> {
    if values.len() <= limit {
        return values;
    }
    if limit == 0 {
        return Vec::new();
    }
    let mut selected = Vec::with_capacity(limit);
    let mut values = values.into_iter().map(Some).collect::<Vec<_>>();
    for slot in 0..limit {
        let index = if limit == 1 {
            values.len() / 2
        } else {
            slot * (values.len() - 1) / (limit - 1)
        };
        if let Some(value) = values[index].take() {
            selected.push(value);
        }
    }
    selected
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
    )
}

fn search_candidates(
    connection: &Connection,
    query: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<Candidate>> {
    let mut candidates = HashMap::<String, Candidate>::new();
    if !terms.is_empty() {
        let expression = terms
            .iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut statement = connection.prepare(
            "SELECT message.message_id, message.sequence, message.occurred_at,
                    message.role, message.content
             FROM transcript_messages_fts
             JOIN transcript_messages AS message
               ON message.message_id = transcript_messages_fts.message_id
             WHERE transcript_messages_fts MATCH ?1
               AND message.retracted_at IS NULL
             ORDER BY bm25(transcript_messages_fts), message.sequence DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![expression, limit as i64], stored_message)?;
        for (rank, row) in rows.enumerate() {
            let message = row?;
            let score = lexical_score(&message.content, query, terms) + 3.0 / (rank + 1) as f64;
            merge_candidate(&mut candidates, message, score);
        }
    }

    let patterns = like_patterns(query, terms);
    if !patterns.is_empty() {
        let predicates = std::iter::repeat_n("LOWER(content) LIKE ? ESCAPE '\\'", patterns.len())
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT message_id, sequence, occurred_at, role, content
             FROM transcript_messages
             WHERE retracted_at IS NULL AND ({predicates})
             ORDER BY sequence DESC LIMIT ?"
        );
        let mut values = patterns
            .iter()
            .map(|pattern| Value::Text(format!("%{}%", escape_like(pattern))))
            .collect::<Vec<_>>();
        values.push(Value::Integer(limit as i64));
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), stored_message)?;
        for row in rows {
            let message = row?;
            let score = lexical_score(&message.content, query, terms);
            merge_candidate(&mut candidates, message, score);
        }
    }

    let mut candidates = candidates.into_values().collect::<Vec<_>>();
    candidates.retain(|candidate| lexical_match(&candidate.message.content, query, terms));
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.message.sequence.cmp(&left.message.sequence))
    });
    candidates.truncate(limit);
    Ok(candidates)
}

fn stored_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let role = row.get::<_, String>(3)?;
    let role = match role.as_str() {
        "user" => MemoryRole::User,
        "assistant" => MemoryRole::Assistant,
        "memory" => MemoryRole::Memory,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown transcript role {role}"),
                )
                .into(),
            ));
        }
    };
    Ok(StoredMessage {
        message_id: row.get(0)?,
        sequence: row.get(1)?,
        occurred_at: row.get(2)?,
        role,
        content: row.get(4)?,
    })
}

fn merge_candidate(
    candidates: &mut HashMap<String, Candidate>,
    message: StoredMessage,
    score: f64,
) {
    candidates
        .entry(message.message_id.clone())
        .and_modify(|candidate| candidate.score = candidate.score.max(score))
        .or_insert(Candidate { message, score });
}

fn like_patterns(query: &str, terms: &[String]) -> Vec<String> {
    let mut patterns = vec![query.to_lowercase()];
    for term in terms {
        if !patterns.contains(term) {
            patterns.push(term.clone());
        }
    }
    patterns.truncate(MAX_TERMS);
    patterns
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn lexical_score(content: &str, query: &str, terms: &[String]) -> f64 {
    let content = content.to_lowercase();
    let mut score = if content.contains(&query.to_lowercase()) {
        8.0
    } else {
        0.0
    };
    for term in terms {
        if content.contains(term) {
            score += 2.0;
        }
    }
    score
}

fn lexical_match(content: &str, query: &str, terms: &[String]) -> bool {
    let content = content.to_lowercase();
    if content.contains(&query.to_lowercase()) {
        return true;
    }
    let matched = terms
        .iter()
        .filter(|term| content.contains(term.as_str()))
        .collect::<Vec<_>>();
    (terms.len() == 1 && matched.len() == 1)
        || matched
            .iter()
            .any(|term| term.chars().count() >= 3 && term.chars().all(is_cjk))
        || matched.len() >= 2
}

fn recurrence_evidence(
    candidates: &[Candidate],
    episode_gap_hours: i64,
) -> TranscriptRecurrenceEvidence {
    let mut messages = candidates
        .iter()
        .filter(|candidate| candidate.message.role == MemoryRole::User)
        .map(|candidate| &candidate.message)
        .collect::<Vec<_>>();
    messages.sort_by_key(|message| message.sequence);
    let distinct_days = messages
        .iter()
        .filter_map(|message| message.occurred_at.get(..10))
        .collect::<BTreeSet<_>>()
        .len();
    let mut distinct_episodes = 0_usize;
    let mut previous: Option<&str> = None;
    for message in &messages {
        let starts_episode = previous
            .map(|at| !within_gap(at, &message.occurred_at, episode_gap_hours))
            .unwrap_or(true);
        if starts_episode {
            distinct_episodes += 1;
        }
        previous = Some(&message.occurred_at);
    }
    TranscriptRecurrenceEvidence {
        user_match_count: messages.len(),
        distinct_day_count: distinct_days,
        distinct_episode_count: distinct_episodes,
        episode_gap_hours,
        first_user_mention_at: messages.first().map(|message| message.occurred_at.clone()),
        last_user_mention_at: messages.last().map(|message| message.occurred_at.clone()),
        source_message_ids: messages
            .iter()
            .map(|message| message.message_id.clone())
            .collect(),
        repeated_across_time: messages.len() >= 2 && (distinct_days >= 2 || distinct_episodes >= 2),
    }
}

fn build_clusters(
    connection: &Connection,
    candidates: &[Candidate],
    options: &BoundedOptions,
) -> Result<Vec<BuiltCluster>> {
    let mut clusters = Vec::<BuiltCluster>::new();
    for candidate in candidates {
        let window = context_window(
            connection,
            candidate.message.sequence,
            options.context_before,
            options.context_after,
            options.episode_gap_hours,
        )?;
        let window_ids = window
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<HashSet<_>>();
        if let Some(cluster) = clusters.iter_mut().find(|cluster| {
            cluster
                .messages
                .values()
                .any(|message| window_ids.contains(&message.message_id))
        }) {
            cluster
                .anchor_ids
                .insert(candidate.message.message_id.clone());
            cluster.score = cluster.score.max(candidate.score);
            cluster.messages.extend(
                window
                    .into_iter()
                    .map(|message| (message.sequence, message)),
            );
            continue;
        }
        if clusters.len() >= options.max_clusters {
            continue;
        }
        let mut cluster = BuiltCluster {
            score: candidate.score,
            anchor_ids: HashSet::from([candidate.message.message_id.clone()]),
            messages: window
                .into_iter()
                .map(|message| (message.sequence, message))
                .collect(),
        };
        for other in candidates {
            if cluster
                .messages
                .values()
                .any(|message| message.message_id == other.message.message_id)
            {
                cluster.anchor_ids.insert(other.message.message_id.clone());
                cluster.score = cluster.score.max(other.score);
            }
        }
        clusters.push(cluster);
    }
    Ok(clusters)
}

fn context_window(
    connection: &Connection,
    anchor_sequence: i64,
    before: usize,
    after: usize,
    episode_gap_hours: i64,
) -> Result<Vec<StoredMessage>> {
    let anchor = connection
        .query_row(
            "SELECT message_id, sequence, occurred_at, role, content
             FROM transcript_messages
             WHERE retracted_at IS NULL AND sequence = ?1",
            [anchor_sequence],
            stored_message,
        )
        .optional()?;
    let Some(anchor) = anchor else {
        return Ok(Vec::new());
    };
    let mut before_statement = connection.prepare(
        "SELECT message_id, sequence, occurred_at, role, content
         FROM transcript_messages
         WHERE retracted_at IS NULL AND sequence < ?1
         ORDER BY sequence DESC LIMIT ?2",
    )?;
    let mut previous = before_statement
        .query_map(params![anchor_sequence, before as i64], stored_message)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    previous.reverse();
    let mut after_statement = connection.prepare(
        "SELECT message_id, sequence, occurred_at, role, content
         FROM transcript_messages
         WHERE retracted_at IS NULL AND sequence > ?1
         ORDER BY sequence LIMIT ?2",
    )?;
    let following = after_statement
        .query_map(params![anchor_sequence, after as i64], stored_message)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut messages = previous;
    messages.push(anchor);
    messages.extend(following);
    let anchor = messages
        .iter()
        .position(|message| message.sequence == anchor_sequence)
        .expect("resolved transcript anchor remains in its window");
    let mut start = anchor;
    while start > 0
        && within_gap(
            &messages[start - 1].occurred_at,
            &messages[start].occurred_at,
            episode_gap_hours,
        )
    {
        start -= 1;
    }
    let mut end = anchor;
    while end + 1 < messages.len()
        && within_gap(
            &messages[end].occurred_at,
            &messages[end + 1].occurred_at,
            episode_gap_hours,
        )
    {
        end += 1;
    }
    Ok(messages[start..=end].to_vec())
}

fn within_gap(left: &str, right: &str, gap_hours: i64) -> bool {
    let Ok(left) = DateTime::parse_from_rfc3339(left) else {
        return false;
    };
    let Ok(right) = DateTime::parse_from_rfc3339(right) else {
        return false;
    };
    let seconds = right.signed_duration_since(left).num_seconds().abs();
    seconds <= gap_hours * 60 * 60
}

fn materialize_clusters(
    clusters: Vec<BuiltCluster>,
    candidate_ids: &HashSet<String>,
    max_clusters: usize,
    max_messages: usize,
    max_chars: usize,
) -> (Vec<TranscriptSearchCluster>, bool) {
    let mut output = Vec::new();
    let mut remaining_messages = max_messages;
    let mut remaining_chars = max_chars;
    let mut truncated = false;
    for cluster in clusters.into_iter().take(max_clusters) {
        if remaining_messages == 0 || remaining_chars == 0 {
            truncated = true;
            break;
        }
        let messages = cluster.messages.into_values().collect::<Vec<_>>();
        let selected = prioritized_indices(&messages, &cluster.anchor_ids, remaining_messages);
        if selected.len() < messages.len() {
            truncated = true;
        }
        let mut materialized = Vec::new();
        for index in selected {
            if remaining_messages == 0 || remaining_chars == 0 {
                truncated = true;
                break;
            }
            let message = &messages[index];
            let allowed = remaining_chars.min(MAX_MESSAGE_CHARS);
            let (content, clipped) = truncate_chars(&message.content, allowed);
            let content_chars = content.chars().count();
            remaining_chars = remaining_chars.saturating_sub(content_chars);
            remaining_messages -= 1;
            truncated |= clipped;
            materialized.push(TranscriptSearchMessage {
                message_id: message.message_id.clone(),
                sequence: message.sequence,
                occurred_at: message.occurred_at.clone(),
                role: message.role.clone(),
                content,
                matched: candidate_ids.contains(&message.message_id),
                truncated: clipped,
            });
        }
        if !materialized.is_empty() {
            let mut source_message_ids = cluster.anchor_ids.into_iter().collect::<Vec<_>>();
            source_message_ids.sort();
            output.push(TranscriptSearchCluster {
                score: cluster.score,
                source_message_ids,
                messages: materialized,
            });
        }
    }
    (output, truncated)
}

fn prioritized_indices(
    messages: &[StoredMessage],
    anchor_ids: &HashSet<String>,
    limit: usize,
) -> Vec<usize> {
    let anchors = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| anchor_ids.contains(&message.message_id).then_some(index))
        .collect::<Vec<_>>();
    let mut selected = BTreeSet::new();
    for index in &anchors {
        if selected.len() < limit {
            selected.insert(*index);
        }
    }
    for distance in 1..messages.len() {
        for anchor in &anchors {
            for index in [anchor.checked_sub(distance), anchor.checked_add(distance)]
                .into_iter()
                .flatten()
                .filter(|index| *index < messages.len())
            {
                if selected.len() < limit {
                    selected.insert(index);
                }
            }
        }
        if selected.len() >= limit {
            break;
        }
    }
    selected.into_iter().collect()
}

fn truncate_chars(content: &str, max_chars: usize) -> (String, bool) {
    if content.chars().count() <= max_chars {
        return (content.to_owned(), false);
    }
    (content.chars().take(max_chars).collect(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{MemoryEntry, MessagePart},
        transcript::{TranscriptMessageLinks, TranscriptStore},
    };

    fn entry(role: MemoryRole, at: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            role,
            at: at.to_owned(),
            content: content.to_owned(),
            revision_id: None,
            parts: vec![MessagePart::Markdown {
                text: content.to_owned(),
            }],
            metadata: None,
            delivery_state: None,
        }
    }

    async fn append(store: &TranscriptStore, role: MemoryRole, at: &str, content: &str) -> String {
        store
            .append(entry(role, at, content), TranscriptMessageLinks::default())
            .await
            .expect("append search fixture")
            .message_id
    }

    #[tokio::test]
    async fn finds_cjk_subjects_with_bounded_same_episode_context() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:00:00Z",
            "很早以前的协作税讨论",
        )
        .await;
        append(
            &store,
            MemoryRole::Assistant,
            "2026-08-02T00:00:00Z",
            "不应跨过一天的间隔",
        )
        .await;
        let anchor = append(
            &store,
            MemoryRole::User,
            "2026-08-02T00:01:00Z",
            "协作税是否来自共享状态竞争？",
        )
        .await;
        append(
            &store,
            MemoryRole::Assistant,
            "2026-08-02T00:02:00Z",
            "这条是同一 episode 的回答",
        )
        .await;

        let result = store
            .search(
                "共享状态竞争",
                TranscriptSearchOptions {
                    max_clusters: 1,
                    context_before: 3,
                    context_after: 3,
                    ..TranscriptSearchOptions::default()
                },
            )
            .await
            .expect("search transcript");

        assert_eq!(result.clusters.len(), 1);
        assert!(result.clusters[0].source_message_ids.contains(&anchor));
        assert_eq!(result.clusters[0].messages.len(), 3);
        assert_eq!(
            result.clusters[0].messages[0].occurred_at,
            "2026-08-02T00:00:00Z"
        );
        assert!(result.clusters[0].messages[1].matched);
    }

    #[tokio::test]
    async fn long_cjk_sentence_recovers_a_short_topic_mention_without_exact_sentence_match() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        let source = append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:00:00Z",
            "上一篇文章反复在说协作税",
        )
        .await;

        let result = store
            .search(
                "我最近重新思考了一个问题，我们应该如何识别协作税长期造成的影响？",
                TranscriptSearchOptions::default(),
            )
            .await
            .expect("search long CJK sentence");

        assert!(result.clusters.iter().any(|cluster| {
            cluster
                .source_message_ids
                .iter()
                .any(|message_id| message_id == &source)
        }));
        assert_eq!(result.recurrence.user_match_count, 1);
    }

    #[tokio::test]
    async fn recurrence_counts_only_user_mentions_across_inferred_episodes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:00:00Z",
            "协作税第一次出现",
        )
        .await;
        append(
            &store,
            MemoryRole::Assistant,
            "2026-08-01T00:01:00Z",
            "协作税由模型重复说明",
        )
        .await;
        append(
            &store,
            MemoryRole::User,
            "2026-08-03T00:00:00Z",
            "后来又主动提到协作税",
        )
        .await;

        let evidence = TranscriptRecall::new(Arc::new(store))
            .recurrence_evidence("协作税", TranscriptSearchOptions::default())
            .await
            .expect("recurrence evidence");
        assert_eq!(evidence.user_match_count, 2);
        assert_eq!(evidence.distinct_day_count, 2);
        assert_eq!(evidence.distinct_episode_count, 2);
        assert!(evidence.repeated_across_time);
        assert_eq!(evidence.source_message_ids.len(), 2);
    }

    #[tokio::test]
    async fn retracted_matches_never_reappear_in_search_or_recurrence() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:00:00Z",
            "保留的另一个主题",
        )
        .await;
        let retracted = append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:01:00Z",
            "应被撤回的协作税",
        )
        .await;
        store
            .retract_from(&retracted)
            .await
            .expect("retract transcript tail");

        let result = store
            .search("协作税", TranscriptSearchOptions::default())
            .await
            .expect("search transcript");
        assert!(result.clusters.is_empty());
        assert_eq!(result.recurrence.user_match_count, 0);
    }

    #[tokio::test]
    async fn clamps_output_and_preserves_the_matching_message() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        append(
            &store,
            MemoryRole::Assistant,
            "2026-08-01T00:00:00Z",
            "long context before anchor",
        )
        .await;
        let anchor = append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:01:00Z",
            "bounded recall target with more content",
        )
        .await;

        let result = store
            .search(
                "recall target",
                TranscriptSearchOptions {
                    max_clusters: 1,
                    max_messages: 1,
                    max_chars: 12,
                    context_before: 2,
                    context_after: 2,
                    episode_gap_hours: 6,
                },
            )
            .await
            .expect("bounded search");
        assert_eq!(result.clusters[0].messages.len(), 1);
        assert_eq!(result.clusters[0].messages[0].message_id, anchor);
        assert_eq!(result.clusters[0].messages[0].content.chars().count(), 12);
        assert!(result.clusters[0].messages[0].truncated);
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn reopening_backfills_an_existing_transcript_index() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("transcript.sqlite3");
        let (store, _) = TranscriptStore::open(path.clone(), None)
            .await
            .expect("open transcript");
        append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:00:00Z",
            "backfill lexical anchor",
        )
        .await;
        drop(store);
        let connection = Connection::open(&path).expect("open fixture database");
        connection
            .execute("DELETE FROM transcript_messages_fts", [])
            .expect("remove derived index row");
        drop(connection);

        let (reopened, _) = TranscriptStore::open(path, None)
            .await
            .expect("reopen transcript");
        let indexed: i64 = Connection::open(reopened.path())
            .expect("inspect reopened transcript")
            .query_row(
                "SELECT COUNT(*) FROM transcript_messages_fts WHERE content MATCH 'backfill'",
                [],
                |row| row.get(0),
            )
            .expect("read rebuilt full-text index");
        assert_eq!(indexed, 1);
        let result = reopened
            .search("lexical anchor", TranscriptSearchOptions::default())
            .await
            .expect("search backfilled index");
        assert_eq!(result.recurrence.user_match_count, 1);
    }

    #[tokio::test]
    async fn resolves_active_retracted_and_unavailable_sources_without_leaking_retracted_text() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (store, _) = TranscriptStore::open(temporary.path().join("transcript.sqlite3"), None)
            .await
            .expect("open transcript");
        let active = append(
            &store,
            MemoryRole::User,
            "2026-08-01T00:00:00Z",
            "active source message with details",
        )
        .await;
        append(
            &store,
            MemoryRole::Assistant,
            "2026-08-01T00:01:00Z",
            "nearby context",
        )
        .await;
        let retracted = append(
            &store,
            MemoryRole::User,
            "2026-08-02T00:00:00Z",
            "private retracted content",
        )
        .await;
        store
            .retract_from(&retracted)
            .await
            .expect("retract source");

        let active_resolution = store
            .resolve_source(
                &active,
                TranscriptSourceOptions {
                    target_max_chars: 8,
                    ..TranscriptSourceOptions::default()
                },
            )
            .await
            .expect("resolve active source");
        assert_eq!(active_resolution.status, TranscriptSourceStatus::Active);
        assert_eq!(active_resolution.messages.len(), 2);
        assert_eq!(active_resolution.messages[0].message_id, active);
        assert_eq!(active_resolution.messages[0].content.chars().count(), 8);
        assert!(active_resolution.messages[0].truncated);

        let retracted_resolution = store
            .resolve_source(&retracted, TranscriptSourceOptions::default())
            .await
            .expect("resolve retracted source");
        assert_eq!(
            retracted_resolution.status,
            TranscriptSourceStatus::Retracted
        );
        assert!(retracted_resolution.messages.is_empty());

        let unavailable = store
            .resolve_source("msg_missing", TranscriptSourceOptions::default())
            .await
            .expect("resolve missing source");
        assert_eq!(unavailable.status, TranscriptSourceStatus::Unavailable);
        assert!(unavailable.messages.is_empty());
    }

    #[test]
    fn rejects_unbounded_queries() {
        assert!(normalize_query("   ").is_err());
        assert!(normalize_query(&"x".repeat(MAX_QUERY_CHARS + 1)).is_err());
    }
}
