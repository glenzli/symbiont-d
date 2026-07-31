use anyhow::{Context, Result};
use pcp_core::{
    LifecycleStatus, PageValidity, PageValidityHint, Projection, SearchHit, SearchMode,
    SearchPagesRequest, SearchResult,
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde_json::{Map, Value};

use crate::{
    row::{REVISION_COLUMNS, revision_from_row},
    store::{MAX_SEARCH_RESULTS, SqlitePcpStore},
    validity::current_validity,
};

impl SqlitePcpStore {
    pub async fn search_pages(&self, mut request: SearchPagesRequest) -> Result<SearchResult> {
        if request.scopes.is_empty() {
            anyhow::bail!("PCP search requires at least one authorized scope");
        }
        request.limit = request.limit.clamp(1, MAX_SEARCH_RESULTS);
        let offset = parse_cursor(request.cursor.as_deref())?;
        self.run("page search", move |connection| {
            if request.mode == SearchMode::Auto {
                let text = search_once(
                    &connection,
                    &request,
                    SearchMode::Text,
                    offset,
                    request.limit as usize,
                );
                if let Ok(result) = text
                    && !result.hits.is_empty()
                {
                    return Ok(result);
                }
                return search_once(
                    &connection,
                    &request,
                    SearchMode::Exact,
                    offset,
                    request.limit as usize,
                );
            }
            search_once(
                &connection,
                &request,
                request.mode.clone(),
                offset,
                request.limit as usize,
            )
        })
        .await
    }
}

fn search_once(
    connection: &Connection,
    request: &SearchPagesRequest,
    mode: SearchMode,
    offset: usize,
    limit: usize,
) -> Result<SearchResult> {
    if mode == SearchMode::Graph {
        return search_graph(connection, request, offset, limit);
    }
    if mode == SearchMode::Summary {
        return search_summaries(connection, request, offset, limit);
    }
    let mut values = Vec::<SqlValue>::new();
    let mut sql = format!(
        "SELECT {REVISION_COLUMNS},
                substr(COALESCE(r.payload_content, r.facets_json, ''), 1, 600),
                EXISTS (
                    SELECT 1 FROM pcp_summary_heads summary_head
                    WHERE summary_head.target_revision_id = r.revision_id
                )
         FROM pcp_pages p
         JOIN pcp_revisions r ON r.revision_id = p.current_revision_id"
    );
    if mode == SearchMode::Text {
        sql.push_str(" JOIN pcp_revision_fts ON pcp_revision_fts.revision_id = r.revision_id");
    }
    sql.push_str(" WHERE r.namespace IN (");
    push_placeholders(&mut sql, request.scopes.len());
    sql.push(')');
    values.extend(request.scopes.iter().cloned().map(SqlValue::Text));

    append_lifecycle_filter(&mut sql, &mut values, request);
    append_time_filters(&mut sql, &mut values, request);
    append_relation_filter(&mut sql, &mut values, request);

    match mode {
        SearchMode::Text => {
            let fts = fts_query(&request.query)
                .context("text search requires at least one searchable term")?;
            sql.push_str(" AND pcp_revision_fts MATCH ?");
            values.push(SqlValue::Text(fts));
        }
        SearchMode::Exact => {
            if !request.query.trim().is_empty() {
                sql.push_str(
                    " AND (
                        instr(lower(COALESCE(r.payload_content, '')), lower(?)) > 0
                        OR instr(lower(COALESCE(r.facets_json, '')), lower(?)) > 0
                    )",
                );
                values.push(SqlValue::Text(request.query.trim().to_owned()));
                values.push(SqlValue::Text(request.query.trim().to_owned()));
            }
        }
        SearchMode::Temporal => {
            if !request.query.trim().is_empty() {
                sql.push_str(
                    " AND (
                        instr(lower(COALESCE(r.payload_content, '')), lower(?)) > 0
                        OR instr(lower(COALESCE(r.facets_json, '')), lower(?)) > 0
                    )",
                );
                values.push(SqlValue::Text(request.query.trim().to_owned()));
                values.push(SqlValue::Text(request.query.trim().to_owned()));
            }
        }
        SearchMode::Auto | SearchMode::Summary | SearchMode::Graph => unreachable!(),
    }
    if mode == SearchMode::Text {
        sql.push_str(
            " ORDER BY bm25(pcp_revision_fts) ASC,
                       COALESCE(r.observed_at, r.created_at) DESC,
                       r.revision_id DESC
              LIMIT ? OFFSET ?",
        );
    } else {
        sql.push_str(
            " ORDER BY COALESCE(r.observed_at, r.created_at) DESC, r.revision_id DESC
              LIMIT ? OFFSET ?",
        );
    }
    values.push(SqlValue::Integer((limit + 1) as i64));
    values.push(SqlValue::Integer(offset as i64));
    collect_hits(
        connection,
        &sql,
        values,
        mode.as_str(),
        if mode == SearchMode::Text {
            "payload"
        } else {
            "payload_or_facets"
        },
        offset,
        limit,
    )
}

fn search_summaries(
    connection: &Connection,
    request: &SearchPagesRequest,
    offset: usize,
    limit: usize,
) -> Result<SearchResult> {
    let fts = fts_query(&request.query);
    let mut values = Vec::<SqlValue>::new();
    let mut sql = format!(
        "SELECT {REVISION_COLUMNS},
                substr(summary.content, 1, 600),
                1
         FROM pcp_pages p
         JOIN pcp_revisions r ON r.revision_id = p.current_revision_id
         JOIN pcp_summary_heads summary_head
           ON summary_head.target_revision_id = r.revision_id
         JOIN pcp_summaries summary
           ON summary.summary_revision_id = summary_head.current_summary_revision_id"
    );
    if fts.is_some() {
        sql.push_str(
            "
            JOIN pcp_summary_fts
              ON pcp_summary_fts.summary_revision_id = summary.summary_revision_id",
        );
    }
    sql.push_str(" WHERE r.namespace IN (");
    push_placeholders(&mut sql, request.scopes.len());
    sql.push(')');
    values.extend(request.scopes.iter().cloned().map(SqlValue::Text));
    append_lifecycle_filter(&mut sql, &mut values, request);
    append_time_filters(&mut sql, &mut values, request);
    append_relation_filter(&mut sql, &mut values, request);
    if let Some(fts) = fts {
        sql.push_str(
            " AND pcp_summary_fts MATCH ?
              ORDER BY bm25(pcp_summary_fts) ASC,
                       COALESCE(r.observed_at, r.created_at) DESC,
                       r.revision_id DESC",
        );
        values.push(SqlValue::Text(fts));
    } else {
        sql.push_str(" ORDER BY COALESCE(r.observed_at, r.created_at) DESC, r.revision_id DESC");
    }
    sql.push_str(" LIMIT ? OFFSET ?");
    values.push(SqlValue::Integer((limit + 1) as i64));
    values.push(SqlValue::Integer(offset as i64));
    collect_hits(
        connection, &sql, values, "summary", "summary", offset, limit,
    )
}

fn search_graph(
    connection: &Connection,
    request: &SearchPagesRequest,
    offset: usize,
    limit: usize,
) -> Result<SearchResult> {
    let query = request.query.trim();
    if query.is_empty() {
        anyhow::bail!("graph search requires a page or revision id");
    }
    let origin_revision = if query.starts_with("pg_") {
        connection
            .query_row(
                "SELECT current_revision_id FROM pcp_pages WHERE page_id = ?1",
                [query],
                |row| row.get::<_, String>(0),
            )
            .context("resolve graph search page")?
    } else {
        query.to_owned()
    };
    ensure_graph_origin_access(connection, &origin_revision, &request.scopes)?;

    let mut values = vec![
        SqlValue::Text(origin_revision.clone()),
        SqlValue::Text(origin_revision.clone()),
        SqlValue::Text(origin_revision),
    ];
    let mut sql = String::from(
        "WITH graph_edges (
            from_revision_id, to_revision_id, edge_type, created_at
         ) AS (
            SELECT from_revision_id, to_revision_id, relation_type, created_at
            FROM pcp_relations
            UNION ALL
            SELECT derived_revision_id, input_revision_id, 'derived_from', created_at
            FROM pcp_provenance_inputs
         ),
         neighbors AS (
            SELECT
                CASE
                    WHEN edge.from_revision_id = ? THEN edge.to_revision_id
                    ELSE edge.from_revision_id
                END AS revision_id,
                MAX(edge.created_at) AS edge_created_at
            FROM graph_edges edge
            WHERE (edge.from_revision_id = ? OR edge.to_revision_id = ?)
              AND edge.from_revision_id <> edge.to_revision_id",
    );
    if !request.filters.relation_types.is_empty() {
        sql.push_str(" AND edge.edge_type IN (");
        push_placeholders(&mut sql, request.filters.relation_types.len());
        sql.push(')');
        values.extend(
            request
                .filters
                .relation_types
                .iter()
                .cloned()
                .map(SqlValue::Text),
        );
    }
    sql.push_str(&format!(
        "
            GROUP BY revision_id
         )
         SELECT {REVISION_COLUMNS},
                substr(COALESCE(r.payload_content, r.facets_json, ''), 1, 600),
                EXISTS (
                    SELECT 1 FROM pcp_summary_heads summary_head
                    WHERE summary_head.target_revision_id = r.revision_id
                )
         FROM neighbors
         JOIN pcp_revisions r ON r.revision_id = neighbors.revision_id
         WHERE r.namespace IN ("
    ));
    push_placeholders(&mut sql, request.scopes.len());
    sql.push(')');
    values.extend(request.scopes.iter().cloned().map(SqlValue::Text));
    append_lifecycle_filter(&mut sql, &mut values, request);
    append_time_filters(&mut sql, &mut values, request);
    sql.push_str(
        " ORDER BY neighbors.edge_created_at DESC, r.revision_id DESC
          LIMIT ? OFFSET ?",
    );
    values.push(SqlValue::Integer((limit + 1) as i64));
    values.push(SqlValue::Integer(offset as i64));
    collect_hits(
        connection,
        &sql,
        values,
        "graph",
        "relations",
        offset,
        limit,
    )
}

fn ensure_graph_origin_access(
    connection: &Connection,
    revision_id: &str,
    allowed_scopes: &[String],
) -> Result<()> {
    let namespace = connection
        .query_row(
            "SELECT namespace FROM pcp_revisions WHERE revision_id = ?1",
            [revision_id],
            |row| row.get::<_, String>(0),
        )
        .with_context(|| format!("find graph origin revision {revision_id}"))?;
    if !allowed_scopes.contains(&namespace) {
        anyhow::bail!("graph origin is outside the authorized PCP scopes");
    }
    Ok(())
}

fn collect_hits(
    connection: &Connection,
    sql: &str,
    values: Vec<SqlValue>,
    matched_by: &str,
    matched_projection: &str,
    offset: usize,
    limit: usize,
) -> Result<SearchResult> {
    let mut statement = connection.prepare(sql).context("prepare PCP search")?;
    let mut rows = statement
        .query(params_from_iter(values.iter()))
        .context("query PCP pages")?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next().context("read PCP search row")? {
        let revision = revision_from_row(row, false, true, false, false)?;
        let snippet: String = row.get(17)?;
        let has_summary: bool = row.get(18)?;
        let validity =
            current_validity(connection, &revision.revision_id)?.map(compact_validity_hint);
        let mut available_projections = vec![
            Projection::Manifest,
            Projection::Payload,
            Projection::Sources,
            Projection::Provenance,
            Projection::Relations,
            Projection::Facets,
            Projection::History,
        ];
        if has_summary {
            available_projections.insert(1, Projection::Summary);
        }
        if validity.is_some() {
            let index = usize::from(has_summary) + 1;
            available_projections.insert(index, Projection::Validity);
        }
        hits.push(SearchHit {
            page_id: revision.page_id,
            revision_id: revision.revision_id,
            namespace: revision.namespace,
            lifecycle_status: revision.lifecycle_status,
            created_at: revision.created_at,
            observed_at: revision.observed_at,
            snippet,
            matched_by: matched_by.to_owned(),
            matched_projection: matched_projection.to_owned(),
            facets: compact_search_facets(revision.facets),
            validity,
            available_projections,
        });
    }
    let has_more = hits.len() > limit;
    hits.truncate(limit);
    Ok(SearchResult {
        hits,
        next_cursor: has_more.then(|| (offset + limit).to_string()),
    })
}

fn compact_validity_hint(validity: PageValidity) -> PageValidityHint {
    PageValidityHint {
        assessment_id: validity.assessment_id,
        standing: validity.standing,
        rationale: truncate_search_text(&validity.rationale, 360),
        scope: validity
            .scope
            .map(|scope| truncate_search_text(&scope, 240)),
        assessed_at: validity.assessed_at,
        basis_revision_count: validity.basis_revision_ids.len() as u32,
    }
}

fn truncate_search_text(content: &str, limit: usize) -> String {
    if content.chars().count() <= limit {
        return content.to_owned();
    }
    let mut compact = content
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    compact.push_str("...");
    compact
}

fn compact_search_facets(facets: Option<Value>) -> Option<Value> {
    facets.map(compact_search_value)
}

fn compact_search_value(value: Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .filter(|(key, _)| !is_payload_bearing_facet(key))
                .take(24)
                .map(|(key, value)| (key, compact_search_value(value)))
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .take(12)
                .map(compact_search_value)
                .collect(),
        ),
        Value::String(content) if content.chars().count() > 240 => {
            let mut compact = content.chars().take(237).collect::<String>();
            compact.push_str("...");
            Value::String(compact)
        }
        other => other,
    }
}

fn is_payload_bearing_facet(key: &str) -> bool {
    matches!(
        key,
        "contentParts" | "messageMetadata" | "contextSnapshot" | "traceEvents"
    )
}

fn append_lifecycle_filter(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    request: &SearchPagesRequest,
) {
    let statuses = if request.filters.lifecycle_status.is_empty() {
        vec![LifecycleStatus::Active]
    } else {
        request.filters.lifecycle_status.clone()
    };
    sql.push_str(" AND r.lifecycle_status IN (");
    push_placeholders(sql, statuses.len());
    sql.push(')');
    values.extend(
        statuses
            .into_iter()
            .map(|status| SqlValue::Text(status.as_str().to_owned())),
    );
}

fn append_time_filters(sql: &mut String, values: &mut Vec<SqlValue>, request: &SearchPagesRequest) {
    if let Some(created_after) = request.filters.created_after.as_ref() {
        sql.push_str(" AND COALESCE(r.observed_at, r.created_at) >= ?");
        values.push(SqlValue::Text(created_after.clone()));
    }
    if let Some(created_before) = request.filters.created_before.as_ref() {
        sql.push_str(" AND COALESCE(r.observed_at, r.created_at) <= ?");
        values.push(SqlValue::Text(created_before.clone()));
    }
}

fn append_relation_filter(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    request: &SearchPagesRequest,
) {
    if request.filters.relation_types.is_empty() {
        return;
    }
    sql.push_str(
        " AND EXISTS (
            SELECT 1 FROM pcp_relations relation_filter
            WHERE (
                relation_filter.from_revision_id = r.revision_id
                OR relation_filter.to_revision_id = r.revision_id
            ) AND relation_filter.relation_type IN (",
    );
    push_placeholders(sql, request.filters.relation_types.len());
    sql.push_str("))");
    values.extend(
        request
            .filters
            .relation_types
            .iter()
            .cloned()
            .map(SqlValue::Text),
    );
}

fn push_placeholders(sql: &mut String, count: usize) {
    for index in 0..count {
        if index > 0 {
            sql.push(',');
        }
        sql.push('?');
    }
}

fn fts_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .map(|term| {
            term.chars()
                .filter(|character| character.is_alphanumeric() || *character == '_')
                .collect::<String>()
        })
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn parse_cursor(cursor: Option<&str>) -> Result<usize> {
    cursor
        .unwrap_or("0")
        .parse::<usize>()
        .context("invalid PCP pagination cursor")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::compact_search_facets;

    #[test]
    fn search_facets_exclude_payload_and_bound_routing_metadata() {
        let facets = compact_search_facets(Some(json!({
            "kind": "conversation_event",
            "role": "user",
            "contentParts": [{"type": "markdown", "text": "full detail"}],
            "messageMetadata": {"contextSnapshot": "large trace"},
            "topic": "x".repeat(300)
        })))
        .expect("compacted facets");

        assert_eq!(facets["kind"], "conversation_event");
        assert_eq!(facets["role"], "user");
        assert!(facets.get("contentParts").is_none());
        assert!(facets.get("messageMetadata").is_none());
        assert_eq!(
            facets["topic"]
                .as_str()
                .expect("compacted topic")
                .chars()
                .count(),
            240
        );
    }
}
