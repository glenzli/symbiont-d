use std::{collections::BTreeSet, sync::Arc};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{
    Actor, ActorType, LifecycleStatus, PagePayload, Projection, ProvenanceEvent, ReadPagesRequest,
    RevisePageRequest, SearchFilters, SearchMode, SearchPagesRequest, SourceRef, WritePageRequest,
    WriteResult,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::continuity::ContinuityHost;

const SYSTEM_ACTOR_ID: &str = "symbiont-d";
const MAX_CONTEXT_CHARS: usize = 32_000;
const MAX_PROMPT_CHARS_PER_DOCUMENT: usize = 4_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextDocumentKind {
    CurrentMap,
    OpenLoops,
    ProfileReview,
}

impl ContextDocumentKind {
    pub fn from_route(value: &str) -> Option<Self> {
        match value {
            "current-map" => Some(Self::CurrentMap),
            "open-loops" => Some(Self::OpenLoops),
            _ => None,
        }
    }

    fn stable_key(self) -> &'static str {
        match self {
            Self::CurrentMap => "symbiont.current_map",
            Self::OpenLoops => "symbiont.open_loops",
            Self::ProfileReview => "symbiont.profile_review",
        }
    }

    fn facet_kind(self) -> &'static str {
        match self {
            Self::CurrentMap => "symbiont_current_map",
            Self::OpenLoops => "symbiont_open_loops",
            Self::ProfileReview => "symbiont_profile_review",
        }
    }

    fn source_uri(self) -> &'static str {
        match self {
            Self::CurrentMap => "symbiont://context/current-map",
            Self::OpenLoops => "symbiont://context/open-loops",
            Self::ProfileReview => "symbiont://context/profile-review",
        }
    }

    fn namespace<'a>(self, continuity: &'a ContinuityHost) -> &'a str {
        match self {
            Self::ProfileReview => continuity.user_scope(),
            Self::CurrentMap | Self::OpenLoops => continuity.project_scope(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDocument {
    pub kind: String,
    pub content: String,
    pub page_id: String,
    pub revision_id: String,
    pub updated_at: String,
    pub source_revision_ids: Vec<String>,
    pub facets: Option<Value>,
}

impl ContextDocument {
    pub fn has_source(&self, revision_id: &str) -> bool {
        self.source_revision_ids
            .iter()
            .any(|source| source == revision_id)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbiontContextSnapshot {
    pub current_map: Option<ContextDocument>,
    pub open_loops: Option<ContextDocument>,
    pub profile_review: Option<ContextDocument>,
}

#[derive(Clone, Copy, Debug)]
pub enum ContextAuthor {
    Model,
    User,
}

impl ContextAuthor {
    fn label(self) -> &'static str {
        match self {
            Self::Model => "Codex",
            Self::User => "local user",
        }
    }

    fn facet(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::User => "user",
        }
    }
}

#[derive(Clone)]
pub struct SymbiontContextStore {
    continuity: Arc<ContinuityHost>,
}

impl SymbiontContextStore {
    pub fn new(continuity: Arc<ContinuityHost>) -> Self {
        Self { continuity }
    }

    pub async fn snapshot(&self) -> Result<SymbiontContextSnapshot> {
        Ok(SymbiontContextSnapshot {
            current_map: self.read(ContextDocumentKind::CurrentMap).await?,
            open_loops: self.read(ContextDocumentKind::OpenLoops).await?,
            profile_review: self.read(ContextDocumentKind::ProfileReview).await?,
        })
    }

    pub async fn read(&self, kind: ContextDocumentKind) -> Result<Option<ContextDocument>> {
        let result = self
            .continuity
            .search(SearchPagesRequest {
                query: kind.stable_key().to_owned(),
                scopes: vec![kind.namespace(&self.continuity).to_owned()],
                mode: SearchMode::Exact,
                projections: vec![Projection::Facets],
                filters: SearchFilters::default(),
                limit: 1,
                cursor: None,
            })
            .await?;
        let Some(hit) = result.hits.into_iter().next() else {
            return Ok(None);
        };
        let mut pages = self
            .continuity
            .read(ReadPagesRequest {
                revision_ids: vec![hit.revision_id],
                projections: vec![
                    Projection::Manifest,
                    Projection::Payload,
                    Projection::Facets,
                    Projection::Provenance,
                ],
                max_chars: MAX_CONTEXT_CHARS as u32,
            })
            .await?;
        let Some(page) = pages.pop() else {
            return Ok(None);
        };
        let revision = page.revision;
        let content = revision
            .payload
            .as_ref()
            .map(|payload| payload.content.clone())
            .unwrap_or_default();
        let source_revision_ids = revision
            .provenance
            .iter()
            .flat_map(|event| event.input_revision_ids.iter().cloned())
            .filter(|source| source != &revision.revision_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(Some(ContextDocument {
            kind: kind.facet_kind().to_owned(),
            content,
            page_id: revision.page_id,
            revision_id: revision.revision_id,
            updated_at: revision.observed_at.unwrap_or(revision.created_at),
            source_revision_ids,
            facets: revision.facets,
        }))
    }

    pub async fn upsert(
        &self,
        kind: ContextDocumentKind,
        content: &str,
        source_revision_ids: Vec<String>,
        extra_facets: Option<Value>,
        author: ContextAuthor,
    ) -> Result<WriteResult> {
        let content = content.trim();
        if content.is_empty() || content.chars().count() > MAX_CONTEXT_CHARS {
            anyhow::bail!("symbiont context must contain 1-{MAX_CONTEXT_CHARS} characters");
        }
        let source_revision_ids = source_revision_ids
            .into_iter()
            .filter(|revision| !revision.trim().is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let facets = context_facets(kind, extra_facets, author);
        let actor = system_actor();
        let observed_at = now();
        let source_refs = vec![SourceRef {
            source_type: "symbiont_context".to_owned(),
            uri: kind.source_uri().to_owned(),
            locator: None,
            metadata: None,
        }];

        if let Some(current) = self.read(kind).await? {
            if current.content.trim() == content && current.facets == Some(facets.clone()) {
                return Ok(WriteResult {
                    page_id: current.page_id,
                    revision_id: current.revision_id,
                    created: false,
                });
            }
            let mut inputs = source_revision_ids;
            if !inputs.contains(&current.revision_id) {
                inputs.push(current.revision_id.clone());
            }
            return self
                .continuity
                .store()
                .revise_page(
                    RevisePageRequest {
                        page_id: current.page_id,
                        expected_revision_id: current.revision_id,
                        created_by: actor.clone(),
                        lifecycle_status: LifecycleStatus::Active,
                        observed_at: Some(observed_at.clone()),
                        valid_from: None,
                        valid_to: None,
                        payload: Some(PagePayload {
                            media_type: "text/markdown".to_owned(),
                            content: content.to_owned(),
                        }),
                        source_refs,
                        facets: Some(facets),
                        provenance: vec![ProvenanceEvent {
                            operation: "revise".to_owned(),
                            actor,
                            timestamp: observed_at,
                            input_revision_ids: inputs,
                            tool_or_model: Some(author.label().to_owned()),
                        }],
                        idempotency_key: None,
                    },
                    self.continuity.allowed_scopes(),
                )
                .await
                .context("revise symbiont context Page");
        }

        self.continuity
            .store()
            .write_page(
                WritePageRequest {
                    owner_id: self.continuity.store().owner_id().to_owned(),
                    namespace: kind.namespace(&self.continuity).to_owned(),
                    visibility: "private".to_owned(),
                    lifecycle_status: LifecycleStatus::Active,
                    created_by: actor.clone(),
                    observed_at: Some(observed_at.clone()),
                    valid_from: None,
                    valid_to: None,
                    payload: Some(PagePayload {
                        media_type: "text/markdown".to_owned(),
                        content: content.to_owned(),
                    }),
                    source_refs,
                    facets: Some(facets),
                    provenance: vec![ProvenanceEvent {
                        operation: "derive".to_owned(),
                        actor,
                        timestamp: observed_at,
                        input_revision_ids: source_revision_ids,
                        tool_or_model: Some(author.label().to_owned()),
                    }],
                    initial_relations: Vec::new(),
                    idempotency_key: Some(format!("symbiont-context:{}", kind.stable_key())),
                },
                self.continuity.allowed_scopes(),
            )
            .await
            .context("write symbiont context Page")
    }

    pub async fn prompt(&self) -> Result<String> {
        let snapshot = self.snapshot().await?;
        let mut sections = Vec::new();
        if let Some(document) = snapshot.current_map {
            sections.push(prompt_section("Current Map", &document));
        }
        if let Some(document) = snapshot.open_loops {
            sections.push(prompt_section("Open Loops", &document));
        }
        if let Some(document) = snapshot.profile_review {
            sections.push(prompt_section("Profile Review", &document));
        }
        if sections.is_empty() {
            return Ok(
                "Symbiont Context has not been curated yet. PCP remains the source archive."
                    .to_owned(),
            );
        }
        Ok(format!(
            "Symbiont Context is a revisable working model derived from PCP, not ground truth. \
             Prefer user-authored evidence when updating it.\n\n{}",
            sections.join("\n\n")
        ))
    }
}

fn context_facets(
    kind: ContextDocumentKind,
    extra_facets: Option<Value>,
    author: ContextAuthor,
) -> Value {
    let mut facets = match extra_facets {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    facets.insert("kind".to_owned(), json!(kind.facet_kind()));
    facets.insert("stableKey".to_owned(), json!(kind.stable_key()));
    facets.insert("authoredBy".to_owned(), json!(author.facet()));
    Value::Object(facets)
}

fn prompt_section(label: &str, document: &ContextDocument) -> String {
    let mut content = document
        .content
        .chars()
        .take(MAX_PROMPT_CHARS_PER_DOCUMENT)
        .collect::<String>();
    if document.content.chars().count() > MAX_PROMPT_CHARS_PER_DOCUMENT {
        content.push_str("\n[truncated; read the PCP Revision for Detail]");
    }
    format!(
        "<{label} revision=\"{}\">\n{}\n</{label}>",
        document.revision_id, content
    )
}

fn system_actor() -> Actor {
    Actor {
        actor_type: ActorType::Tool,
        actor_id: SYSTEM_ACTOR_ID.to_owned(),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_sqlite::SqlitePcpStore;

    use super::{ContextAuthor, ContextDocumentKind, SymbiontContextStore};
    use crate::{
        continuity::{ContinuityHost, MessageLinks},
        memory::MemoryRole,
    };

    #[tokio::test]
    async fn revises_one_stable_context_page_with_exact_sources() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-context-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP"),
        );
        let continuity = Arc::new(ContinuityHost::open(pcp).await.expect("open continuity"));
        let context = SymbiontContextStore::new(Arc::clone(&continuity));
        let first_source = continuity
            .ingest_message(
                MemoryRole::User,
                "The current focus is memory routing.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store first source");
        let first = context
            .upsert(
                ContextDocumentKind::CurrentMap,
                "# Current\n\nMemory routing.",
                vec![first_source.page.revision_id.clone()],
                None,
                ContextAuthor::Model,
            )
            .await
            .expect("write current map");
        let second_source = continuity
            .ingest_message(
                MemoryRole::User,
                "The focus now includes the agent runtime.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store second source");
        let revised = context
            .upsert(
                ContextDocumentKind::CurrentMap,
                "# Current\n\nMemory routing and agent runtime.",
                vec![second_source.page.revision_id.clone()],
                None,
                ContextAuthor::Model,
            )
            .await
            .expect("revise current map");

        assert_eq!(first.page_id, revised.page_id);
        assert_ne!(first.revision_id, revised.revision_id);
        let document = context
            .read(ContextDocumentKind::CurrentMap)
            .await
            .expect("read current map")
            .expect("current map exists");
        assert!(document.has_source(&second_source.page.revision_id));
        assert_eq!(
            document.content,
            "# Current\n\nMemory routing and agent runtime."
        );

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
