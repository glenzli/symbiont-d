use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
#[cfg(test)]
use pcp_client::EmbeddedPcpClient;
use pcp_client::{DurablePageInventoryItem, PcpApi};
use pcp_core::{
    AccessPrincipal, AccessPrincipalType, AccessSession, Actor, ActorType,
    AssessPageValidityRequest, ConsolidatePagesRequest, ConsolidationInput, CreateScopeRequest,
    InitialRelation, LifecycleStatus, LinkPagesRequest, PageMutability, PagePayload, Projection,
    ProvenanceEvent, ReadPage, ReadPagesRequest, Relation, RevisePageRequest, Scope, SearchFilters,
    SearchMode, SearchPagesRequest, SearchResult, SourceRef, ValidityStanding, WritePageRequest,
    WriteResult, WriteSummaryRequest, WriteSummaryResult, WriteValidityResult,
};
#[cfg(test)]
use pcp_store::PcpStore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::{
    asset::{ImageAttachment, SavedImage},
    conversation_projection::ConversationProjection,
    memory::{
        MemoryEntry, MemoryRole, MemoryStore, MessageDeliveryState, MessageMetadata, MessagePart,
        MessageQuote, MessageQuoteDraft, MessageTopicReference,
    },
    profile::{ProfileSnapshot, SetupStatus},
    working_context::{WORKING_CONTEXT_SCAN_MESSAGES, WorkingContext},
};

const USER_SCOPE_LABEL: &str = "User context";
pub const MAX_QUOTES_PER_MESSAGE: usize = 6;
pub const MAX_QUOTE_TEXT_CHARS: usize = 6_000;
const PROJECT_NAMESPACE: &str = "project:symbiont-d";
const CONVERSATION_NAMESPACE: &str = "conversation:symbiont-d-main";
const MODEL_ACTOR_ID: &str = "codex:symbiont-d";
const SYSTEM_ACTOR_ID: &str = "symbiont-d";
const MAX_MODEL_WRITE_CHARS: usize = 64_000;
const INDEX_EXCLUDED_PAGE_KINDS: &[&str] = &[
    "conversation_event",
    "summary_projection",
    "symbiont_current_map",
    "symbiont_open_loops",
    "symbiont_profile_review",
    "symbiont_hunch",
    "user_orientation",
    "conversation_checkpoint",
    "image_asset",
    "tombstone",
];
const SUMMARY_EXCLUDED_PAGE_KINDS: &[&str] = &[
    "summary_projection",
    "symbiont_current_map",
    "symbiont_open_loops",
    "symbiont_profile_review",
    "symbiont_hunch",
    "user_orientation",
    "conversation_checkpoint",
    "image_asset",
    "tombstone",
];

#[derive(Clone, Debug)]
pub struct ScopePolicy {
    pub user: String,
    pub project: String,
    pub conversation: String,
}

impl ScopePolicy {
    fn for_owner(owner_id: &str) -> Self {
        Self {
            user: format!("user:{owner_id}"),
            project: PROJECT_NAMESPACE.to_owned(),
            conversation: CONVERSATION_NAMESPACE.to_owned(),
        }
    }

    pub fn all(&self) -> Vec<String> {
        vec![
            self.user.clone(),
            self.project.clone(),
            self.conversation.clone(),
        ]
    }
}

pub struct ContinuityHost {
    store: Arc<dyn PcpApi>,
    scopes: ScopePolicy,
    event_counter: AtomicU64,
    orientation: RwLock<Option<WriteResult>>,
    live_conversation: ConversationProjection,
}

#[derive(Clone, Debug, Default)]
pub struct MessageLinks {
    pub responds_to: Option<String>,
    pub continues_from: Option<String>,
    pub input_revision_ids: Vec<String>,
    pub surfaced_hunch_revision_ids: Vec<String>,
    pub quotes: Vec<MessageQuote>,
    pub topic: Option<MessageTopicReference>,
}

#[derive(Clone, Debug)]
pub struct StoredMessage {
    pub entry: MemoryEntry,
    pub page: WriteResult,
    pub attachment_revision_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MessageRetractionResult {
    pub retracted_revision_ids: Vec<String>,
    pub message_revision_ids: Vec<String>,
    pub restored_page_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ImageAssetPage {
    pub revision_id: String,
    pub observed_at: String,
    pub attachment: ImageAttachment,
    pub attached_to_revision_id: Option<String>,
    pub source_type: Option<String>,
    pub revised_prompt: Option<String>,
}

impl ContinuityHost {
    pub fn access_session(owner_id: &str) -> AccessSession {
        AccessSession::full_control(
            AccessPrincipal {
                principal_id: "host:symbiont-d".to_owned(),
                principal_type: AccessPrincipalType::Host,
                display_name: Some("symbiont-d".to_owned()),
            },
            format!("symbiont-d:{}", std::process::id()),
            ScopePolicy::for_owner(owner_id).all(),
        )
    }

    pub async fn open(store: Arc<dyn PcpApi>) -> Result<Self> {
        let owner_id = store.owner_id().to_owned();
        let scopes = ScopePolicy::for_owner(&owner_id);
        for request in [
            CreateScopeRequest {
                owner_id: owner_id.clone(),
                namespace: scopes.user.clone(),
                display_name: USER_SCOPE_LABEL.to_owned(),
                description: Some("Long-lived context explicitly owned by the user.".to_owned()),
                parent_namespace: None,
                visibility: "private".to_owned(),
            },
            CreateScopeRequest {
                owner_id: owner_id.clone(),
                namespace: scopes.project.clone(),
                display_name: "symbiont-d".to_owned(),
                description: Some("Project-level context for symbiont-d.".to_owned()),
                parent_namespace: Some(scopes.user.clone()),
                visibility: "private".to_owned(),
            },
            CreateScopeRequest {
                owner_id,
                namespace: scopes.conversation.clone(),
                display_name: "symbiont-d main conversation".to_owned(),
                description: Some(
                    "Raw user, assistant, and tool events from this companion.".to_owned(),
                ),
                parent_namespace: Some(scopes.project.clone()),
                visibility: "private".to_owned(),
            },
        ] {
            store.create_scope(request).await?;
        }
        Ok(Self {
            store,
            scopes,
            event_counter: AtomicU64::new(0),
            orientation: RwLock::new(None),
            live_conversation: ConversationProjection::new(),
        })
    }

    #[cfg(test)]
    pub async fn open_embedded_for_test(store: Arc<dyn PcpStore>) -> Result<Self> {
        let access = Self::access_session(store.owner_id());
        Self::open(EmbeddedPcpClient::shared(store, access)).await
    }

    pub fn store(&self) -> &dyn PcpApi {
        self.store.as_ref()
    }

    pub fn allowed_scopes(&self) -> Vec<String> {
        self.scopes.all()
    }

    pub fn conversation_scope(&self) -> &str {
        &self.scopes.conversation
    }

    pub fn user_scope(&self) -> &str {
        &self.scopes.user
    }

    pub fn project_scope(&self) -> &str {
        &self.scopes.project
    }

    pub fn resolve_scopes(&self, requested: &[String]) -> Result<Vec<String>> {
        let allowed = self.scopes.all();
        if requested.is_empty() {
            return Ok(allowed);
        }
        for scope in requested {
            if !allowed.contains(scope) {
                anyhow::bail!("scope is not authorized for this symbiont session");
            }
        }
        Ok(requested.to_vec())
    }

    pub async fn context_seed(&self, current: Option<&StoredMessage>) -> String {
        let orientation = self
            .orientation
            .read()
            .await
            .as_ref()
            .map(|page| page.revision_id.clone())
            .unwrap_or_else(|| "none".to_owned());
        let checkpoint = self
            .latest_checkpoint_revision()
            .await
            .unwrap_or_else(|| "none".to_owned());
        let current_revision = current
            .map(|message| message.page.revision_id.as_str())
            .unwrap_or("none");
        let mut seed = format!(
            "PCP boundary: `{}` is the complete symbiont-d transcript across native-thread \
             resets and compactions; native context is recent only. Current event: \
             `{current_revision}`; orientation: `{orientation}`; latest checkpoint: \
             `{checkpoint}`. Search then selectively read older context before relying on it or \
             asking the user to repeat it. Derived Pages may use `{}` or `{}`.",
            self.scopes.conversation, self.scopes.user, self.scopes.project
        );
        if let Some(attachments) = current
            .map(|message| &message.attachment_revision_ids)
            .filter(|attachments| !attachments.is_empty())
        {
            seed.push_str(&format!(
                " Current image Revisions: {}.",
                attachments.join(", ")
            ));
        }
        seed
    }

    async fn latest_checkpoint_revision(&self) -> Option<String> {
        self.search(SearchPagesRequest {
            query: "conversation_checkpoint".to_owned(),
            scopes: vec![self.scopes.conversation.clone()],
            mode: SearchMode::Exact,
            term_match: pcp_core::SearchTermMatch::All,
            projections: vec![Projection::Facets],
            filters: SearchFilters::default(),
            limit: 1,
            cursor: None,
        })
        .await
        .ok()?
        .hits
        .into_iter()
        .next()
        .map(|hit| hit.revision_id)
    }

    pub async fn migrate_legacy(
        &self,
        memory: &MemoryStore,
        profile: &ProfileSnapshot,
    ) -> Result<MigrationSummary> {
        let entries = memory.all_entries().await?;
        let mut migrated_messages = 0_u64;
        let mut source_revisions = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let actor = actor_for_role(&entry.role);
            let result = self
                .store
                .write_page(WritePageRequest {
                    owner_id: self.store.owner_id().to_owned(),
                    namespace: self.scopes.conversation.clone(),
                    visibility: "private".to_owned(),
                    lifecycle_status: LifecycleStatus::Active,
                    kind: "conversation_event".to_owned(),
                    mutability: PageMutability::Sealed,
                    created_by: actor.clone(),
                    observed_at: Some(entry.at.clone()),
                    valid_from: None,
                    valid_to: None,
                    payload: Some(PagePayload {
                        media_type: "text/markdown".to_owned(),
                        content: entry.content.clone(),
                    }),
                    source_refs: vec![SourceRef {
                        source_type: "legacy_markdown_memory".to_owned(),
                        uri: memory.source_uri(),
                        locator: Some(format!("entry:{index}")),
                        metadata: None,
                    }],
                    facets: Some(message_facets(&entry)),
                    provenance: vec![ProvenanceEvent {
                        operation: "import".to_owned(),
                        actor: system_actor(),
                        timestamp: now(),
                        input_revision_ids: Vec::new(),
                        tool_or_model: Some("symbiont legacy importer".to_owned()),
                    }],
                    initial_relations: Vec::new(),
                    idempotency_key: Some(format!("legacy-memory:{}:{index}", entry.at)),
                })
                .await?;
            source_revisions.push(result.revision_id);
            migrated_messages += u64::from(result.created);
        }
        let orientation = self
            .sync_orientation(profile, source_revisions)
            .await
            .context("migrate visible orientation into PCP")?;
        Ok(MigrationSummary {
            migrated_messages,
            orientation,
        })
    }

    pub async fn sync_orientation(
        &self,
        profile: &ProfileSnapshot,
        source_revision_ids: Vec<String>,
    ) -> Result<Option<WriteResult>> {
        if profile.status != SetupStatus::Ready || profile.orientation.trim().is_empty() {
            return Ok(None);
        }
        let actor = system_actor();
        let initial = self
            .store
            .write_page(WritePageRequest {
                owner_id: self.store.owner_id().to_owned(),
                namespace: self.scopes.user.clone(),
                visibility: "private".to_owned(),
                lifecycle_status: LifecycleStatus::Active,
                kind: "user_orientation".to_owned(),
                mutability: PageMutability::Revisioned,
                created_by: actor.clone(),
                observed_at: profile.updated_at.clone(),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: profile.orientation.trim().to_owned(),
                }),
                source_refs: vec![SourceRef {
                    source_type: "symbiont_orientation".to_owned(),
                    uri: "symbiont://profile/orientation".to_owned(),
                    locator: None,
                    metadata: None,
                }],
                facets: Some(json!({
                    "kind": "user_orientation",
                    "stableKey": "symbiont.orientation"
                })),
                provenance: vec![ProvenanceEvent {
                    operation: "derive".to_owned(),
                    actor: actor.clone(),
                    timestamp: now(),
                    input_revision_ids: source_revision_ids,
                    tool_or_model: Some("symbiont onboarding".to_owned()),
                }],
                initial_relations: Vec::new(),
                idempotency_key: Some("symbiont.orientation".to_owned()),
            })
            .await?;
        let current_revision = self
            .store
            .current_revision_id(initial.page_id.clone())
            .await?;
        let current = self
            .store
            .read_pages(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: vec![current_revision.clone()],
                projections: vec![Projection::Payload],
                max_chars: 64_000,
            })
            .await?;
        let current_content = current
            .first()
            .and_then(|page| page.revision.payload.as_ref())
            .map(|payload| payload.content.trim())
            .unwrap_or_default();
        if current_content == profile.orientation.trim() {
            let current = WriteResult {
                page_id: initial.page_id,
                revision_id: current_revision,
                created: initial.created,
            };
            *self.orientation.write().await = Some(current.clone());
            return Ok(Some(current));
        }
        let previous_revision = current_revision.clone();
        let revised = self
            .store
            .revise_page(RevisePageRequest {
                page_id: initial.page_id,
                expected_revision_id: current_revision,
                created_by: actor.clone(),
                lifecycle_status: LifecycleStatus::Active,
                observed_at: profile.updated_at.clone(),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: profile.orientation.trim().to_owned(),
                }),
                source_refs: vec![SourceRef {
                    source_type: "symbiont_orientation".to_owned(),
                    uri: "symbiont://profile/orientation".to_owned(),
                    locator: None,
                    metadata: None,
                }],
                facets: Some(json!({
                    "kind": "user_orientation",
                    "stableKey": "symbiont.orientation"
                })),
                provenance: vec![ProvenanceEvent {
                    operation: "revise".to_owned(),
                    actor,
                    timestamp: now(),
                    input_revision_ids: vec![previous_revision],
                    tool_or_model: Some("symbiont profile editor".to_owned()),
                }],
                initial_relations: Vec::new(),
                idempotency_key: None,
            })
            .await?;
        *self.orientation.write().await = Some(revised.clone());
        Ok(Some(revised))
    }

    pub async fn recent_source_revisions(&self, limit: u32) -> Result<Vec<String>> {
        let result = self
            .search(SearchPagesRequest {
                query: "conversation_event".to_owned(),
                scopes: vec![self.scopes.conversation.clone()],
                mode: SearchMode::Exact,
                term_match: pcp_core::SearchTermMatch::All,
                projections: vec![Projection::Facets],
                filters: SearchFilters::default(),
                limit: limit.min(50),
                cursor: None,
            })
            .await?;
        Ok(result.hits.into_iter().map(|hit| hit.revision_id).collect())
    }

    pub async fn ingest_message(
        &self,
        role: MemoryRole,
        content: &str,
        images: Vec<SavedImage>,
        metadata: Option<MessageMetadata>,
        links: MessageLinks,
    ) -> Result<StoredMessage> {
        let content = content.trim();
        if content.is_empty() && images.is_empty() && links.quotes.is_empty() {
            anyhow::bail!("conversation event requires text, an image, or a quote");
        }
        let observed_at = now();
        let actor = actor_for_role(&role);
        let tool_or_model = metadata
            .as_ref()
            .and_then(|metadata| metadata.runs.last())
            .map(|run| run.model.clone());
        let attachment_revision_ids = self
            .ingest_image_assets(&images, &observed_at, &actor, tool_or_model.as_deref())
            .await
            .context("ingest image assets into PCP")?;
        let mut parts = Vec::with_capacity(
            images.len()
                + links.quotes.len()
                + usize::from(links.topic.is_some())
                + usize::from(!content.is_empty()),
        );
        if let Some(topic) = links.topic.clone() {
            parts.push(MessagePart::Topic { topic });
        }
        parts.extend(
            links
                .quotes
                .iter()
                .cloned()
                .map(|quote| MessagePart::Quote { quote }),
        );
        if !content.is_empty() {
            parts.push(MessagePart::Markdown {
                text: content.to_owned(),
            });
        }
        parts.extend(images.iter().map(|image| MessagePart::Image {
            asset: image.attachment.clone(),
        }));
        let mut entry = MemoryEntry {
            role: role.clone(),
            at: observed_at,
            content: content.to_owned(),
            revision_id: None,
            parts,
            metadata,
            delivery_state: None,
        };
        let event_key = self.next_event_key();
        let mut relation_targets = Vec::new();
        if let Some(revision_id) = links.responds_to.as_ref() {
            relation_targets.push(("responds_to".to_owned(), revision_id.clone()));
        }
        if let Some(revision_id) = links.continues_from.as_ref() {
            relation_targets.push(("continues".to_owned(), revision_id.clone()));
        }
        relation_targets.extend(
            links
                .surfaced_hunch_revision_ids
                .iter()
                .cloned()
                .map(|revision_id| ("surfaces_hunch".to_owned(), revision_id)),
        );
        relation_targets.extend(
            attachment_revision_ids
                .iter()
                .cloned()
                .map(|revision_id| ("has_attachment".to_owned(), revision_id)),
        );
        let mut quoted_revision_ids = links
            .quotes
            .iter()
            .map(|quote| quote.source_revision_id.clone())
            .collect::<Vec<_>>();
        quoted_revision_ids.sort();
        quoted_revision_ids.dedup();
        relation_targets.extend(
            quoted_revision_ids
                .iter()
                .cloned()
                .map(|revision_id| ("quotes".to_owned(), revision_id)),
        );
        let initial_relations = self
            .initial_relations_for_revision_targets(relation_targets)
            .await?;
        let mut provenance_inputs = links.input_revision_ids;
        provenance_inputs.extend(links.surfaced_hunch_revision_ids);
        provenance_inputs.extend(quoted_revision_ids);
        if let Some(revision_id) = links.responds_to {
            provenance_inputs.push(revision_id);
        }
        if let Some(revision_id) = links.continues_from {
            provenance_inputs.push(revision_id);
        }
        provenance_inputs.sort();
        provenance_inputs.dedup();
        let payload_content = if entry.content.is_empty() && !images.is_empty() {
            images
                .iter()
                .map(|image| format!("[Image: {}]", image.attachment.filename))
                .collect::<Vec<_>>()
                .join("\n")
        } else if entry.content.is_empty() {
            links
                .quotes
                .iter()
                .map(|quote| {
                    format!(
                        "[Quoted {}: {}]",
                        quote.source_revision_id,
                        quote.text.replace('\n', " ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            entry.content.clone()
        };
        let page = self
            .store
            .write_page(WritePageRequest {
                owner_id: self.store.owner_id().to_owned(),
                namespace: self.scopes.conversation.clone(),
                visibility: "private".to_owned(),
                lifecycle_status: LifecycleStatus::Active,
                kind: "conversation_event".to_owned(),
                mutability: PageMutability::Sealed,
                created_by: actor.clone(),
                observed_at: Some(entry.at.clone()),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: payload_content,
                }),
                source_refs: Vec::new(),
                facets: Some(message_facets(&entry)),
                provenance: vec![ProvenanceEvent {
                    operation: "ingest".to_owned(),
                    actor,
                    timestamp: entry.at.clone(),
                    input_revision_ids: provenance_inputs,
                    tool_or_model,
                }],
                initial_relations,
                idempotency_key: Some(event_key),
            })
            .await?;
        entry.revision_id = Some(page.revision_id.clone());
        self.live_conversation.publish(entry.clone()).await;
        Ok(StoredMessage {
            entry,
            page,
            attachment_revision_ids,
        })
    }

    async fn ingest_image_assets(
        &self,
        images: &[SavedImage],
        observed_at: &str,
        actor: &Actor,
        tool_or_model: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut revision_ids = Vec::with_capacity(images.len());
        for image in images {
            let payload = serde_json::to_string_pretty(&image.attachment)?;
            let result = self
                .store
                .write_page(WritePageRequest {
                    owner_id: self.store.owner_id().to_owned(),
                    namespace: self.scopes.conversation.clone(),
                    visibility: "private".to_owned(),
                    lifecycle_status: LifecycleStatus::Active,
                    kind: "image_asset".to_owned(),
                    mutability: PageMutability::Sealed,
                    created_by: actor.clone(),
                    observed_at: Some(observed_at.to_owned()),
                    valid_from: None,
                    valid_to: None,
                    payload: Some(PagePayload {
                        media_type: "application/vnd.symbiont.image+json".to_owned(),
                        content: payload,
                    }),
                    source_refs: vec![SourceRef {
                        source_type: image.source_type().to_owned(),
                        uri: image.source_uri().to_owned(),
                        locator: None,
                        metadata: image.source_metadata(),
                    }],
                    facets: Some(json!({
                        "kind": "image_asset",
                        "sha256": image.attachment.sha256,
                        "origin": image.source_type()
                    })),
                    provenance: vec![ProvenanceEvent {
                        operation: "ingest".to_owned(),
                        actor: actor.clone(),
                        timestamp: observed_at.to_owned(),
                        input_revision_ids: Vec::new(),
                        tool_or_model: tool_or_model.map(str::to_owned),
                    }],
                    initial_relations: Vec::new(),
                    idempotency_key: Some(format!("image-asset:{}", image.attachment.sha256)),
                })
                .await?;
            revision_ids.push(result.revision_id);
        }
        Ok(revision_ids)
    }

    pub async fn recent_image_assets(&self, limit: usize) -> Result<Vec<ImageAssetPage>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let result = self
            .search(SearchPagesRequest {
                query: "image_asset".to_owned(),
                scopes: vec![self.scopes.conversation.clone()],
                mode: SearchMode::Exact,
                term_match: pcp_core::SearchTermMatch::All,
                projections: vec![Projection::Facets],
                filters: SearchFilters::default(),
                limit: limit.min(20) as u32,
                cursor: None,
            })
            .await?;
        let revision_ids = result
            .hits
            .into_iter()
            .map(|hit| hit.revision_id)
            .collect::<Vec<_>>();
        self.read_image_assets(&revision_ids).await
    }

    pub async fn read_image_assets(&self, revision_ids: &[String]) -> Result<Vec<ImageAssetPage>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        if revision_ids.len() > 20 {
            anyhow::bail!("at most 20 image asset Revisions can be read at once");
        }
        let pages = self
            .read(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: revision_ids.to_vec(),
                projections: vec![
                    Projection::Payload,
                    Projection::Sources,
                    Projection::Relations,
                    Projection::Facets,
                ],
                max_chars: 64_000,
            })
            .await?;
        let mut by_revision = pages
            .into_iter()
            .map(|page| {
                let revision_id = page.revision.revision_id.clone();
                image_asset_from_page(page)
                    .with_context(|| format!("Revision {revision_id} is not an image asset"))
                    .map(|image| (revision_id, image))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        revision_ids
            .iter()
            .map(|revision_id| {
                by_revision
                    .remove(revision_id)
                    .with_context(|| format!("image asset Revision {revision_id} was not found"))
            })
            .collect()
    }

    pub async fn resolve_message_quotes(
        &self,
        drafts: Vec<MessageQuoteDraft>,
    ) -> Result<Vec<MessageQuote>> {
        if drafts.len() > MAX_QUOTES_PER_MESSAGE {
            anyhow::bail!("a message can quote at most {MAX_QUOTES_PER_MESSAGE} excerpts");
        }
        if drafts.is_empty() {
            return Ok(Vec::new());
        }
        for draft in &drafts {
            if draft.source_revision_id.trim().is_empty() || draft.source_revision_id.len() > 128 {
                anyhow::bail!("quoted source Revision ID is invalid");
            }
            if draft.start_offset.is_some() != draft.end_offset.is_some()
                || draft
                    .start_offset
                    .zip(draft.end_offset)
                    .is_some_and(|(start, end)| end < start)
            {
                anyhow::bail!("quoted selection offsets are invalid");
            }
            let selected_chars = draft.selected_text.trim().chars().count();
            if !draft.whole_message && selected_chars == 0 {
                anyhow::bail!("a selected quote cannot be empty");
            }
            if selected_chars > MAX_QUOTE_TEXT_CHARS {
                anyhow::bail!("quoted text exceeds {MAX_QUOTE_TEXT_CHARS} characters");
            }
        }

        let mut revision_ids = drafts
            .iter()
            .map(|draft| draft.source_revision_id.clone())
            .collect::<Vec<_>>();
        revision_ids.sort();
        revision_ids.dedup();
        let pages = self
            .read(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids,
                projections: vec![Projection::Payload, Projection::Facets],
                max_chars: (MAX_QUOTES_PER_MESSAGE * (MAX_QUOTE_TEXT_CHARS + 2_000)) as u32,
            })
            .await?
            .into_iter()
            .map(|page| (page.revision.revision_id.clone(), page))
            .collect::<HashMap<_, _>>();

        drafts
            .into_iter()
            .map(|draft| {
                let page = pages.get(&draft.source_revision_id).with_context(|| {
                    format!(
                        "quoted conversation Revision {} was not found",
                        draft.source_revision_id
                    )
                })?;
                let source_role = page_message_role(&page)
                    .context("quoted Revision is not a conversation message")?;
                let source_at = page
                    .revision
                    .observed_at
                    .clone()
                    .unwrap_or_else(|| page.revision.created_at.clone());
                let source = page
                    .revision
                    .payload
                    .as_ref()
                    .context("quoted conversation Revision has no readable content")?
                    .content
                    .clone();
                let source_sha256 = format!("{:x}", Sha256::digest(source.as_bytes()));
                let (text, truncated) = if draft.whole_message {
                    truncate_with_flag(&source, MAX_QUOTE_TEXT_CHARS)
                } else {
                    (draft.selected_text.trim().to_owned(), false)
                };
                Ok(MessageQuote {
                    source_revision_id: draft.source_revision_id,
                    source_role,
                    source_at,
                    text,
                    source_sha256,
                    start_offset: draft.start_offset,
                    end_offset: draft.end_offset,
                    whole_message: draft.whole_message,
                    truncated,
                })
            })
            .collect()
    }

    pub async fn attached_image_revision_ids(
        &self,
        message_revision_ids: &[String],
    ) -> Result<Vec<String>> {
        if message_revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        if message_revision_ids.len() > 20 {
            anyhow::bail!("at most 20 message Revisions can be inspected at once");
        }
        let pages = self
            .read(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: message_revision_ids.to_vec(),
                projections: vec![Projection::Relations],
                max_chars: 16_000,
            })
            .await?;
        let message_page_ids = pages
            .iter()
            .map(|page| page.page.page_id.clone())
            .collect::<HashSet<_>>();
        let mut image_page_ids = Vec::new();
        for relation in pages.into_iter().flat_map(|page| page.relations) {
            if relation.relation_type == "has_attachment"
                && message_page_ids.contains(&relation.from_page_id)
                && !image_page_ids.contains(&relation.to_page_id)
            {
                image_page_ids.push(relation.to_page_id);
            }
        }
        if image_page_ids.is_empty() {
            return Ok(Vec::new());
        }
        let image_pages = self
            .read(ReadPagesRequest {
                page_ids: image_page_ids.clone(),
                revision_ids: Vec::new(),
                projections: vec![Projection::Manifest],
                max_chars: 256,
            })
            .await?;
        let by_page = image_pages
            .into_iter()
            .map(|page| (page.page.page_id, page.revision.revision_id))
            .collect::<HashMap<_, _>>();
        Ok(image_page_ids
            .into_iter()
            .filter_map(|page_id| by_page.get(&page_id).cloned())
            .collect())
    }

    pub async fn recent_messages(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let mut cursor = None;
        let mut revision_ids = Vec::new();
        while revision_ids.len() < limit {
            let remaining = limit - revision_ids.len();
            let result = self
                .search(SearchPagesRequest {
                    query: "conversation_event".to_owned(),
                    scopes: vec![self.scopes.conversation.clone()],
                    mode: SearchMode::Exact,
                    term_match: pcp_core::SearchTermMatch::All,
                    projections: vec![Projection::Facets],
                    filters: SearchFilters::default(),
                    limit: remaining.min(50) as u32,
                    cursor: cursor.clone(),
                })
                .await?;
            revision_ids.extend(result.hits.into_iter().map(|hit| hit.revision_id));
            cursor = result.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        revision_ids.truncate(limit);
        let mut pages = Vec::with_capacity(revision_ids.len());
        for chunk in revision_ids.chunks(20) {
            pages.extend(
                self.read(ReadPagesRequest {
                    page_ids: Vec::new(),
                    revision_ids: chunk.to_vec(),
                    projections: vec![
                        Projection::Payload,
                        Projection::Facets,
                        Projection::Relations,
                    ],
                    max_chars: 64_000,
                })
                .await?,
            );
        }
        let revision_page_ids = pages
            .iter()
            .map(|page| (page.revision.revision_id.clone(), page.page.page_id.clone()))
            .collect::<HashMap<_, _>>();
        let active_assistant_page_ids = pages
            .iter()
            .filter(|page| page_message_role(page) == Some(MemoryRole::Assistant))
            .map(|page| page.page.page_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let replied_to_page_ids = pages
            .iter()
            .flat_map(|page| page.relations.iter())
            .filter(|relation| {
                relation.relation_type == "responds_to"
                    && active_assistant_page_ids.contains(&relation.from_page_id)
            })
            .map(|relation| relation.to_page_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut entries = Vec::with_capacity(pages.len());
        for page in pages {
            if let Some(mut entry) = memory_entry_from_page(page) {
                if entry.role == MemoryRole::User {
                    entry.delivery_state = Some(MessageDeliveryState::Delivered);
                }
                entries.push(entry);
            }
        }
        entries.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.revision_id.cmp(&right.revision_id))
        });
        if let Some(latest_user) = entries
            .iter_mut()
            .rev()
            .find(|entry| entry.role == MemoryRole::User)
            && latest_user
                .revision_id
                .as_ref()
                .and_then(|revision| revision_page_ids.get(revision))
                .is_some_and(|page_id| !replied_to_page_ids.contains(page_id))
        {
            latest_user.delivery_state = Some(MessageDeliveryState::Failed);
        }
        Ok(entries)
    }

    pub async fn messages_by_revision_ids(
        &self,
        revision_ids: &[String],
    ) -> Result<Vec<MemoryEntry>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        if revision_ids.len() > 200 {
            anyhow::bail!("at most 200 conversation Revisions can be read at once");
        }
        let mut unique = revision_ids.to_vec();
        unique.sort();
        unique.dedup();
        let mut entries = Vec::with_capacity(unique.len());
        for chunk in unique.chunks(20) {
            let pages = self
                .read(ReadPagesRequest {
                    page_ids: Vec::new(),
                    revision_ids: chunk.to_vec(),
                    projections: vec![Projection::Payload, Projection::Facets],
                    max_chars: 128_000,
                })
                .await?;
            entries.extend(pages.into_iter().filter_map(memory_entry_from_page));
        }
        entries.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.revision_id.cmp(&right.revision_id))
        });
        Ok(entries)
    }

    pub async fn retract_user_message_and_after(
        &self,
        revision_id: &str,
    ) -> Result<MessageRetractionResult> {
        let messages = self.recent_messages(500).await?;
        let target_index = messages
            .iter()
            .position(|entry| entry.revision_id.as_deref() == Some(revision_id))
            .context("message is not an active conversation event")?;
        if messages[target_index].role != MemoryRole::User {
            anyhow::bail!("only user messages can be retracted");
        }

        let suffix_revisions = messages[target_index..]
            .iter()
            .filter_map(|entry| entry.revision_id.clone())
            .collect::<Vec<_>>();
        let mut retracted_revision_ids = Vec::new();
        let mut restored_page_ids = Vec::new();
        for root_revision_id in suffix_revisions.iter().rev() {
            let cascade = self
                .store
                .tombstone_derivation_cascade(root_revision_id.clone(), system_actor())
                .await?;
            retracted_revision_ids.extend(cascade.retracted_revision_ids);
            restored_page_ids.extend(cascade.restored_page_ids);
        }
        retracted_revision_ids.sort();
        retracted_revision_ids.dedup();
        restored_page_ids.sort();
        restored_page_ids.dedup();

        let mut message_revision_ids = Vec::new();
        for chunk in retracted_revision_ids.chunks(20) {
            let pages = self
                .read(ReadPagesRequest {
                    page_ids: Vec::new(),
                    revision_ids: chunk.to_vec(),
                    projections: vec![Projection::Facets],
                    max_chars: 16_000,
                })
                .await?;
            message_revision_ids.extend(pages.into_iter().filter_map(|page| {
                (page
                    .revision
                    .facets
                    .as_ref()
                    .and_then(|facets| facets.get("kind"))
                    .and_then(Value::as_str)
                    == Some("conversation_event"))
                .then_some(page.revision.revision_id)
            }));
        }
        message_revision_ids.sort();
        message_revision_ids.dedup();
        self.live_conversation.remove(&message_revision_ids).await;
        Ok(MessageRetractionResult {
            retracted_revision_ids,
            message_revision_ids,
            restored_page_ids,
        })
    }

    pub async fn live_messages_after(
        &self,
        after_revision_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        Ok(self.live_conversation.after(after_revision_id, limit).await)
    }

    pub async fn latest_assistant_revision(&self) -> Result<Option<String>> {
        Ok(self
            .recent_messages(1)
            .await?
            .into_iter()
            .next()
            .filter(|entry| entry.role == MemoryRole::Assistant)
            .and_then(|entry| entry.revision_id))
    }

    pub async fn working_context(
        &self,
        cursor_before: Option<&str>,
        current_revision_id: Option<&str>,
        reply_to_revision_id: Option<&str>,
    ) -> Result<WorkingContext> {
        let entries = self.recent_messages(WORKING_CONTEXT_SCAN_MESSAGES).await?;
        Ok(WorkingContext::build(
            &entries,
            cursor_before,
            current_revision_id,
            reply_to_revision_id,
        ))
    }

    pub async fn memory_chars(&self) -> Result<usize> {
        self.store
            .content_char_count(vec![self.scopes.conversation.clone()])
            .await
    }

    pub async fn list_scopes(
        &self,
        query: Option<String>,
        limit: u32,
        cursor: Option<String>,
    ) -> Result<(Vec<Scope>, Option<String>)> {
        self.store
            .list_scopes(self.allowed_scopes(), query, limit, cursor)
            .await
    }

    pub async fn search(&self, mut request: SearchPagesRequest) -> Result<SearchResult> {
        request.scopes = self.resolve_scopes(&request.scopes)?;
        self.store.search_pages(request).await
    }

    pub async fn browse_index(
        &self,
        requested_scopes: &[String],
        limit: u32,
        cursor: Option<String>,
        max_chars: u32,
    ) -> Result<SearchResult> {
        self.store
            .browse_index(
                self.resolve_scopes(requested_scopes)?,
                owned_page_kinds(INDEX_EXCLUDED_PAGE_KINDS),
                limit,
                cursor,
                max_chars,
            )
            .await
    }

    pub async fn read(&self, request: ReadPagesRequest) -> Result<Vec<ReadPage>> {
        self.store.read_pages(request).await
    }

    pub async fn current_revision_id(&self, page_id: &str) -> Result<String> {
        self.store.current_revision_id(page_id.to_owned()).await
    }

    pub async fn page_ids_for_revisions(
        &self,
        revision_ids: &[String],
    ) -> Result<HashMap<String, String>> {
        let mut unique = revision_ids.to_vec();
        unique.sort();
        unique.dedup();
        let mut resolved = HashMap::with_capacity(unique.len());
        for chunk in unique.chunks(20) {
            let pages = self
                .read(ReadPagesRequest {
                    page_ids: Vec::new(),
                    revision_ids: chunk.to_vec(),
                    projections: vec![Projection::Manifest],
                    max_chars: 256,
                })
                .await?;
            for page in pages {
                resolved.insert(page.revision.revision_id, page.page.page_id);
            }
        }
        if resolved.len() != unique.len() {
            let missing = unique
                .into_iter()
                .filter(|revision_id| !resolved.contains_key(revision_id))
                .collect::<Vec<_>>();
            anyhow::bail!("PCP Revisions were not found: {}", missing.join(", "));
        }
        Ok(resolved)
    }

    pub async fn initial_relations_for_revision_targets(
        &self,
        targets: Vec<(String, String)>,
    ) -> Result<Vec<InitialRelation>> {
        let revision_ids = targets
            .iter()
            .map(|(_, revision_id)| revision_id.clone())
            .collect::<Vec<_>>();
        let page_ids = self.page_ids_for_revisions(&revision_ids).await?;
        targets
            .into_iter()
            .map(|(relation_type, revision_id)| {
                Ok(InitialRelation {
                    relation_type,
                    to_page_id: page_ids
                        .get(&revision_id)
                        .context("resolved PCP Revision lost its Page identity")?
                        .clone(),
                    basis_revision_ids: vec![revision_id],
                })
            })
            .collect()
    }

    pub async fn surfaced_hunch_revisions(&self, message_revision_id: &str) -> Result<Vec<String>> {
        let mut pages = self
            .read(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: vec![message_revision_id.to_owned()],
                projections: vec![Projection::Relations],
                max_chars: 8_000,
            })
            .await?;
        let Some(message) = pages.pop() else {
            return Ok(Vec::new());
        };
        let mut page_ids = message
            .relations
            .into_iter()
            .filter(|relation| {
                relation.from_page_id == message.page.page_id
                    && relation.relation_type == "surfaces_hunch"
            })
            .map(|relation| relation.to_page_id)
            .collect::<Vec<_>>();
        page_ids.sort();
        page_ids.dedup();
        if page_ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .read(ReadPagesRequest {
                page_ids,
                revision_ids: Vec::new(),
                projections: vec![Projection::Manifest],
                max_chars: 256,
            })
            .await?
            .into_iter()
            .map(|page| page.revision.revision_id)
            .collect())
    }

    pub async fn next_summary_candidate(&self, minimum_chars: usize) -> Result<Option<String>> {
        self.store
            .next_summary_candidate(minimum_chars, owned_page_kinds(SUMMARY_EXCLUDED_PAGE_KINDS))
            .await
    }

    pub async fn durable_page_inventory(&self) -> Result<Vec<DurablePageInventoryItem>> {
        self.store
            .durable_page_inventory(owned_page_kinds(INDEX_EXCLUDED_PAGE_KINDS))
            .await
    }

    pub async fn mark_summary_assessed(
        &self,
        target_revision_id: String,
        outcome: &str,
        tool_or_model: Option<String>,
    ) -> Result<()> {
        self.store
            .mark_summary_assessed(target_revision_id, outcome.to_owned(), tool_or_model)
            .await
    }

    pub async fn write_model_page(
        &self,
        namespace: Option<&str>,
        content: &str,
        facets: Option<Value>,
        source_refs: Vec<SourceRef>,
        source_revision_ids: Vec<String>,
        relations: Vec<InitialRelation>,
        idempotency_key: Option<String>,
    ) -> Result<WriteResult> {
        if content.trim().is_empty() || content.chars().count() > MAX_MODEL_WRITE_CHARS {
            anyhow::bail!("model Page content must contain 1-{MAX_MODEL_WRITE_CHARS} characters");
        }
        let namespace = namespace.unwrap_or(&self.scopes.user);
        self.resolve_scopes(&[namespace.to_owned()])?;
        let actor = model_actor();
        let mut relations = relations;
        let source_relations = self
            .initial_relations_for_revision_targets(
                source_revision_ids
                    .iter()
                    .cloned()
                    .map(|revision_id| ("derived_from".to_owned(), revision_id))
                    .collect(),
            )
            .await?;
        for source_relation in source_relations {
            if !relations.iter().any(|relation| {
                relation.relation_type == source_relation.relation_type
                    && relation.to_page_id == source_relation.to_page_id
            }) {
                relations.push(source_relation);
            }
        }
        let kind = page_kind(facets.as_ref(), "memory_synthesis");
        self.store
            .write_page(WritePageRequest {
                owner_id: self.store.owner_id().to_owned(),
                namespace: namespace.to_owned(),
                visibility: "private".to_owned(),
                lifecycle_status: LifecycleStatus::Active,
                kind,
                mutability: PageMutability::Revisioned,
                created_by: actor.clone(),
                observed_at: Some(now()),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: content.trim().to_owned(),
                }),
                source_refs,
                facets,
                provenance: vec![ProvenanceEvent {
                    operation: "derive".to_owned(),
                    actor,
                    timestamp: now(),
                    input_revision_ids: source_revision_ids,
                    tool_or_model: Some("Codex".to_owned()),
                }],
                initial_relations: relations,
                idempotency_key,
            })
            .await
    }

    pub async fn write_model_summary(
        &self,
        target_page_id: String,
        target_revision_id: String,
        expected_summary_revision_id: Option<String>,
        content: String,
        source_revision_ids: Vec<String>,
        idempotency_key: Option<String>,
        tool_or_model: Option<String>,
    ) -> Result<WriteSummaryResult> {
        let actor = model_actor();
        let tool_or_model = tool_or_model.unwrap_or_else(|| "Codex".to_owned());
        let mut inputs = Vec::with_capacity(source_revision_ids.len() + 1);
        inputs.push(target_revision_id.clone());
        for revision_id in source_revision_ids {
            if !inputs.contains(&revision_id) {
                inputs.push(revision_id);
            }
        }
        self.store
            .write_summary(WriteSummaryRequest {
                target_page_id,
                target_revision_id,
                expected_summary_revision_id,
                content,
                created_by: actor.clone(),
                tool_or_model: Some(tool_or_model.clone()),
                provenance: vec![ProvenanceEvent {
                    operation: "summarize".to_owned(),
                    actor,
                    timestamp: now(),
                    input_revision_ids: inputs,
                    tool_or_model: Some(tool_or_model),
                }],
                idempotency_key,
            })
            .await
    }

    pub async fn revise_current_model_page(
        &self,
        target_page_id: String,
        expected_revision_id: String,
        content: String,
        source_revision_ids: Vec<String>,
    ) -> Result<WriteResult> {
        let target = self
            .store
            .read_pages(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: vec![expected_revision_id.clone()],
                projections: vec![Projection::Manifest, Projection::Facets],
                max_chars: 256,
            })
            .await?
            .into_iter()
            .next()
            .context("revision target is not available")?;
        if target.page.page_id != target_page_id {
            anyhow::bail!("target Page and expected Revision do not match");
        }
        self.revise_model_page(
            target_page_id,
            expected_revision_id,
            content,
            target.revision.facets,
            Vec::new(),
            LifecycleStatus::Active,
            source_revision_ids,
            None,
        )
        .await
    }

    pub async fn consolidate_model_pages(
        &self,
        canonical_page_id: String,
        expected_canonical_revision_id: String,
        replaced_pages: Vec<ConsolidationInput>,
        content: String,
        tool_or_model: Option<String>,
    ) -> Result<WriteResult> {
        if content.trim().is_empty() || content.chars().count() > MAX_MODEL_WRITE_CHARS {
            anyhow::bail!(
                "consolidated Page content must contain 1-{MAX_MODEL_WRITE_CHARS} characters"
            );
        }
        let mut replaced_revision_ids = replaced_pages
            .iter()
            .map(|input| input.expected_revision_id.clone())
            .collect::<Vec<_>>();
        replaced_revision_ids.push(expected_canonical_revision_id.clone());
        replaced_revision_ids.sort();
        replaced_revision_ids.dedup();
        if replaced_revision_ids.len() < 2 {
            anyhow::bail!("consolidation requires at least two distinct current Pages");
        }
        let consolidation_pages = self
            .store
            .read_pages(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids: replaced_revision_ids.clone(),
                projections: vec![
                    Projection::Manifest,
                    Projection::Sources,
                    Projection::Facets,
                ],
                max_chars: 8_000,
            })
            .await?;
        let canonical = consolidation_pages
            .iter()
            .find(|page| page.revision.revision_id == expected_canonical_revision_id)
            .context("canonical consolidation Page is not available")?;
        if canonical.page.page_id != canonical_page_id {
            anyhow::bail!("canonical Page and expected Revision do not match");
        }
        for replacement in &replaced_pages {
            let replaced = consolidation_pages
                .iter()
                .find(|page| page.revision.revision_id == replacement.expected_revision_id)
                .with_context(|| {
                    format!(
                        "replacement consolidation Revision {} is not available",
                        replacement.expected_revision_id
                    )
                })?;
            if replaced.page.page_id != replacement.page_id {
                anyhow::bail!("replacement Page and expected Revision do not match");
            }
            for identity_key in ["stableKey", "episodeId"] {
                let canonical_identity = canonical
                    .revision
                    .facets
                    .as_ref()
                    .and_then(|facets| facets.get(identity_key))
                    .and_then(Value::as_str)
                    .filter(|identity| !identity.trim().is_empty());
                let replaced_identity = replaced
                    .revision
                    .facets
                    .as_ref()
                    .and_then(|facets| facets.get(identity_key))
                    .and_then(Value::as_str)
                    .filter(|identity| !identity.trim().is_empty());
                if let (Some(canonical_identity), Some(replaced_identity)) =
                    (canonical_identity, replaced_identity)
                    && canonical_identity != replaced_identity
                {
                    anyhow::bail!(
                        "consolidation cannot merge conflicting Host identity {identity_key}"
                    );
                }
            }
        }
        let actor = model_actor();
        let timestamp = now();
        let mut facets = canonical
            .revision
            .facets
            .clone()
            .unwrap_or_else(|| json!({}));
        if let Some(object) = facets.as_object_mut() {
            object
                .entry("kind")
                .or_insert_with(|| Value::String("memory_synthesis".to_owned()));
        }
        let mut digest = Sha256::new();
        for revision_id in &replaced_revision_ids {
            digest.update(revision_id.as_bytes());
            digest.update([0]);
        }
        digest.update(content.trim().as_bytes());
        let idempotency_key = format!("model-consolidation:{:x}", digest.finalize());
        self.store
            .consolidate_pages(ConsolidatePagesRequest {
                canonical_page_id,
                expected_canonical_revision_id,
                replaced_pages,
                created_by: actor.clone(),
                lifecycle_status: LifecycleStatus::Active,
                observed_at: Some(timestamp.clone()),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: content.trim().to_owned(),
                }),
                source_refs: canonical.revision.source_refs.clone(),
                facets: Some(facets),
                provenance: vec![ProvenanceEvent {
                    operation: "consolidate".to_owned(),
                    actor,
                    timestamp,
                    input_revision_ids: replaced_revision_ids,
                    tool_or_model: Some(tool_or_model.unwrap_or_else(|| "Codex".to_owned())),
                }],
                idempotency_key: Some(idempotency_key),
            })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn assess_model_page_validity(
        &self,
        target_page_id: String,
        target_revision_id: String,
        expected_assessment_id: Option<String>,
        standing: ValidityStanding,
        rationale: String,
        scope: Option<String>,
        mut basis_revision_ids: Vec<String>,
        idempotency_key: Option<String>,
        tool_or_model: Option<String>,
    ) -> Result<WriteValidityResult> {
        basis_revision_ids.sort();
        basis_revision_ids.dedup();
        self.store
            .assess_page_validity(AssessPageValidityRequest {
                target_page_id,
                target_revision_id,
                expected_assessment_revision_id: expected_assessment_id,
                standing,
                rationale,
                scope,
                basis_revision_ids,
                created_by: model_actor(),
                tool_or_model: Some(tool_or_model.unwrap_or_else(|| "Codex".to_owned())),
                idempotency_key,
            })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn revise_model_page(
        &self,
        page_id: String,
        expected_revision_id: String,
        content: String,
        facets: Option<Value>,
        source_refs: Vec<SourceRef>,
        lifecycle_status: LifecycleStatus,
        source_revision_ids: Vec<String>,
        idempotency_key: Option<String>,
    ) -> Result<WriteResult> {
        if content.trim().is_empty() || content.chars().count() > MAX_MODEL_WRITE_CHARS {
            anyhow::bail!("model Page content must contain 1-{MAX_MODEL_WRITE_CHARS} characters");
        }
        let actor = model_actor();
        let mut provenance_inputs = Vec::with_capacity(source_revision_ids.len() + 1);
        provenance_inputs.push(expected_revision_id.clone());
        provenance_inputs.extend(source_revision_ids.iter().cloned());
        self.store
            .revise_page(RevisePageRequest {
                page_id,
                expected_revision_id,
                created_by: actor.clone(),
                lifecycle_status,
                observed_at: Some(now()),
                valid_from: None,
                valid_to: None,
                payload: Some(PagePayload {
                    media_type: "text/markdown".to_owned(),
                    content: content.trim().to_owned(),
                }),
                source_refs,
                facets,
                provenance: vec![ProvenanceEvent {
                    operation: "revise".to_owned(),
                    actor,
                    timestamp: now(),
                    input_revision_ids: provenance_inputs,
                    tool_or_model: Some("Codex".to_owned()),
                }],
                initial_relations: Vec::new(),
                idempotency_key,
            })
            .await
    }

    pub async fn link_model_pages(
        &self,
        from_page_id: String,
        relation_type: String,
        to_page_id: String,
        basis_revision_ids: Vec<String>,
        idempotency_key: Option<String>,
    ) -> Result<Relation> {
        self.store
            .link_pages(LinkPagesRequest {
                from_page_id,
                relation_type,
                to_page_id,
                basis_revision_ids,
                created_by: model_actor(),
                idempotency_key,
            })
            .await
    }

    fn next_event_key(&self) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = self.event_counter.fetch_add(1, Ordering::Relaxed);
        format!("symbiont-event:{nanos}:{counter}")
    }
}

#[derive(Clone, Debug)]
pub struct MigrationSummary {
    pub migrated_messages: u64,
    pub orientation: Option<WriteResult>,
}

fn actor_for_role(role: &MemoryRole) -> Actor {
    match role {
        MemoryRole::User => Actor {
            actor_type: ActorType::User,
            actor_id: "local-user".to_owned(),
        },
        MemoryRole::Assistant => model_actor(),
        MemoryRole::Memory => Actor {
            actor_type: ActorType::Tool,
            actor_id: SYSTEM_ACTOR_ID.to_owned(),
        },
    }
}

fn model_actor() -> Actor {
    Actor {
        actor_type: ActorType::Model,
        actor_id: MODEL_ACTOR_ID.to_owned(),
    }
}

fn truncate_with_flag(value: &str, max_chars: usize) -> (String, bool) {
    if value.chars().count() <= max_chars {
        return (value.to_owned(), false);
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    (truncated, true)
}

fn system_actor() -> Actor {
    Actor {
        actor_type: ActorType::System,
        actor_id: SYSTEM_ACTOR_ID.to_owned(),
    }
}

fn owned_page_kinds(kinds: &[&str]) -> Vec<String> {
    kinds.iter().map(|kind| (*kind).to_owned()).collect()
}

fn page_kind(facets: Option<&Value>, fallback: &str) -> String {
    facets
        .and_then(|facets| facets.get("kind"))
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn message_facets(entry: &MemoryEntry) -> Value {
    json!({
        "kind": "conversation_event",
        "role": match entry.role {
            MemoryRole::User => "user",
            MemoryRole::Assistant => "assistant",
            MemoryRole::Memory => "memory",
        },
        "messageMetadata": entry.metadata,
        "contentParts": entry.parts
    })
}

fn memory_entry_from_page(page: ReadPage) -> Option<MemoryEntry> {
    let facets = page.revision.facets?;
    if facets.get("kind").and_then(Value::as_str) != Some("conversation_event") {
        return None;
    }
    let role = match facets.get("role").and_then(Value::as_str)? {
        "user" => MemoryRole::User,
        "assistant" => MemoryRole::Assistant,
        "memory" => MemoryRole::Memory,
        _ => return None,
    };
    let metadata = facets
        .get("messageMetadata")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let payload = page.revision.payload?;
    let mut parts = facets
        .get("contentParts")
        .and_then(|value| serde_json::from_value::<Vec<MessagePart>>(value.clone()).ok())
        .unwrap_or_default();
    if parts.is_empty() {
        parts.push(MessagePart::Markdown {
            text: payload.content.clone(),
        });
    }
    Some(MemoryEntry {
        role,
        at: page
            .revision
            .observed_at
            .unwrap_or(page.revision.created_at),
        content: payload.content,
        revision_id: Some(page.revision.revision_id),
        parts,
        metadata,
        delivery_state: None,
    })
}

fn image_asset_from_page(page: ReadPage) -> Option<ImageAssetPage> {
    let page_id = page.page.page_id;
    let revision = page.revision;
    if revision
        .facets
        .as_ref()?
        .get("kind")
        .and_then(Value::as_str)
        != Some("image_asset")
    {
        return None;
    }
    let payload = revision.payload.as_ref()?;
    if payload.media_type != "application/vnd.symbiont.image+json" {
        return None;
    }
    let attachment = serde_json::from_str::<ImageAttachment>(&payload.content).ok()?;
    let attached_to_revision_id = page
        .relations
        .iter()
        .find(|relation| {
            relation.relation_type == "has_attachment" && relation.to_page_id == page_id
        })
        .and_then(|relation| {
            relation
                .basis_revision_ids
                .iter()
                .find(|basis_revision_id| *basis_revision_id != &revision.revision_id)
                .cloned()
        });
    let source_type = revision
        .source_refs
        .first()
        .map(|source| source.source_type.clone());
    let revised_prompt = revision
        .source_refs
        .iter()
        .filter_map(|source| source.metadata.as_ref())
        .find_map(|metadata| {
            metadata
                .get("revisedPrompt")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Some(ImageAssetPage {
        revision_id: revision.revision_id,
        observed_at: revision.observed_at.unwrap_or(revision.created_at),
        attachment,
        attached_to_revision_id,
        source_type,
        revised_prompt,
    })
}

fn page_message_role(page: &ReadPage) -> Option<MemoryRole> {
    let facets = page.revision.facets.as_ref()?;
    if facets.get("kind").and_then(Value::as_str) != Some("conversation_event") {
        return None;
    }
    match facets.get("role").and_then(Value::as_str)? {
        "user" => Some(MemoryRole::User),
        "assistant" => Some(MemoryRole::Assistant),
        "memory" => Some(MemoryRole::Memory),
        _ => None,
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "continuity/tests.rs"]
mod tests;
