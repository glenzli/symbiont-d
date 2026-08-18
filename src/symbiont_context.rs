use std::sync::Arc;

use anyhow::Result;
use pcp_core::WriteResult;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::symbiont_state::{LocalContextDocument, SymbiontStateStore};

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
    fn facet(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::User => "user",
        }
    }
}

#[derive(Clone)]
pub struct SymbiontContextStore {
    state: Arc<SymbiontStateStore>,
}

impl SymbiontContextStore {
    pub fn from_state(state: Arc<SymbiontStateStore>) -> Self {
        Self { state }
    }

    /// Retains narrow test compatibility for call sites that previously built
    /// an embedded PCP host.  Production code must use [`Self::from_state`].
    #[cfg(test)]
    pub fn new(_legacy_continuity: Arc<crate::continuity::ContinuityHost>) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-context-test-{nonce}.sqlite3"));
        Self::from_state(Arc::new(SymbiontStateStore::for_test(path)))
    }

    pub async fn snapshot(&self) -> Result<SymbiontContextSnapshot> {
        Ok(SymbiontContextSnapshot {
            current_map: self.read(ContextDocumentKind::CurrentMap).await?,
            open_loops: self.read(ContextDocumentKind::OpenLoops).await?,
            profile_review: self.read(ContextDocumentKind::ProfileReview).await?,
        })
    }

    pub async fn read(&self, kind: ContextDocumentKind) -> Result<Option<ContextDocument>> {
        self.state
            .read_context(kind.stable_key())
            .await
            .map(|document| document.map(context_document))
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
        let facets = context_facets(kind, extra_facets, author);
        let (document, created, _) = self
            .state
            .upsert_context(
                kind.stable_key(),
                kind.facet_kind(),
                content,
                source_revision_ids,
                Some(facets),
            )
            .await?;
        Ok(WriteResult {
            page_id: document.document_id,
            revision_id: document.revision_id,
            created,
        })
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
                "Symbiont Context has not been curated yet. Do not infer long-term priorities from this absence."
                    .to_owned(),
            );
        }
        Ok(format!(
            "Symbiont Context is a local, revisable working model, not ground truth. \
             Prefer user-authored evidence when updating it.\n\n{}",
            sections.join("\n\n")
        ))
    }

    pub async fn exploration_prompt(&self) -> Result<String> {
        let snapshot = self.snapshot().await?;
        let mut sections = Vec::new();
        if let Some(document) = snapshot.current_map {
            sections.push(prompt_section("Current Map", &document));
        }
        if let Some(document) = snapshot.open_loops {
            sections.push(prompt_section("Open Loops", &document));
        }
        if sections.is_empty() {
            return Ok(
                "No curated Current Map or Open Loops are available. Do not infer priorities from this absence."
                    .to_owned(),
            );
        }
        Ok(format!(
            "This is a bounded exploration projection, not a combined task queue. Current Map is \
             the user's active project and decision landscape; use it only to recognize possible \
             consequences. Open Loops are user-facing unresolved matters, not instructions to \
             investigate every item. Profile Review is intentionally excluded because the profile \
             orientation is supplied separately and maintenance evidence should not steer discovery.\n\n{}",
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
        content.push_str("\n[truncated; read the local Symbiont state for detail]");
    }
    format!(
        "<{label} revision=\"{}\">\n{}\n</{label}>",
        document.revision_id, content
    )
}

fn context_document(document: LocalContextDocument) -> ContextDocument {
    ContextDocument {
        kind: document.kind,
        content: document.content,
        page_id: document.document_id,
        revision_id: document.revision_id,
        updated_at: document.updated_at,
        source_revision_ids: document.source_revision_ids,
        facets: document.facets,
    }
}

// This is the former v0.7 revision test.  v0.8 tenant ingress deliberately
// has no revisable context Page capability, so it must not exercise a
// privileged embedded store as if it represented the live contract.
#[cfg(any())]
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
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
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

        context
            .upsert(
                ContextDocumentKind::ProfileReview,
                "# Maintenance only\n\nDo not use this as a discovery priority.",
                vec![first_source.page.revision_id],
                None,
                ContextAuthor::Model,
            )
            .await
            .expect("write profile review");
        let generic_prompt = context.prompt().await.expect("render generic prompt");
        let exploration_prompt = context
            .exploration_prompt()
            .await
            .expect("render exploration prompt");
        assert!(generic_prompt.contains("Maintenance only"));
        assert!(exploration_prompt.contains("Memory routing and agent runtime"));
        assert!(!exploration_prompt.contains("Maintenance only"));

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
