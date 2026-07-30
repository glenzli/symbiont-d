use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use pcp_core::{
    Actor, ActorType, CreateScopeRequest, InitialRelation, LifecycleStatus, LinkPagesRequest,
    PagePayload, Projection, ProvenanceEvent, ReadPage, ReadPagesRequest, Relation,
    RevisePageRequest, Scope, SearchFilters, SearchMode, SearchPagesRequest, SearchResult,
    SourceRef, WritePageRequest, WriteResult, WriteSummaryRequest, WriteSummaryResult,
};
use pcp_sqlite::{SqlitePcpStore, TombstoneCascadeResult};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};

use crate::{
    asset::SavedImage,
    memory::{
        MemoryEntry, MemoryRole, MemoryStore, MessageDeliveryState, MessageMetadata, MessagePart,
    },
    profile::{ProfileSnapshot, SetupStatus},
    working_context::{WORKING_CONTEXT_SCAN_MESSAGES, WorkingContext},
};

const USER_SCOPE_LABEL: &str = "User context";
const PROJECT_NAMESPACE: &str = "project:symbiont-d";
const CONVERSATION_NAMESPACE: &str = "conversation:symbiont-d-main";
const MODEL_ACTOR_ID: &str = "codex:symbiont-d";
const SYSTEM_ACTOR_ID: &str = "symbiont-d";
const MAX_MODEL_WRITE_CHARS: usize = 64_000;

#[derive(Clone, Debug)]
pub struct ScopePolicy {
    pub user: String,
    pub project: String,
    pub conversation: String,
}

impl ScopePolicy {
    pub fn all(&self) -> Vec<String> {
        vec![
            self.user.clone(),
            self.project.clone(),
            self.conversation.clone(),
        ]
    }
}

pub struct ContinuityHost {
    store: Arc<SqlitePcpStore>,
    scopes: ScopePolicy,
    event_counter: AtomicU64,
    orientation: RwLock<Option<WriteResult>>,
    last_event_revision: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct MessageLinks {
    pub responds_to: Option<String>,
    pub input_revision_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StoredMessage {
    pub entry: MemoryEntry,
    pub page: WriteResult,
    pub attachment_revision_ids: Vec<String>,
}

impl ContinuityHost {
    pub async fn open(store: Arc<SqlitePcpStore>) -> Result<Self> {
        let owner_id = store.owner_id().to_owned();
        let scopes = ScopePolicy {
            user: format!("user:{owner_id}"),
            project: PROJECT_NAMESPACE.to_owned(),
            conversation: CONVERSATION_NAMESPACE.to_owned(),
        };
        for request in [
            CreateScopeRequest {
                owner_id: owner_id.clone(),
                namespace: scopes.user.clone(),
                scope_type: "user".to_owned(),
                display_name: USER_SCOPE_LABEL.to_owned(),
                description: Some("Long-lived context explicitly owned by the user.".to_owned()),
                parent_namespace: None,
                visibility: "private".to_owned(),
            },
            CreateScopeRequest {
                owner_id: owner_id.clone(),
                namespace: scopes.project.clone(),
                scope_type: "project".to_owned(),
                display_name: "symbiont-d".to_owned(),
                description: Some("Project-level context for symbiont-d.".to_owned()),
                parent_namespace: Some(scopes.user.clone()),
                visibility: "private".to_owned(),
            },
            CreateScopeRequest {
                owner_id,
                namespace: scopes.conversation.clone(),
                scope_type: "conversation".to_owned(),
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
            last_event_revision: Mutex::new(None),
        })
    }

    pub fn store(&self) -> &Arc<SqlitePcpStore> {
        &self.store
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
        let mut previous_revision: Option<String> = None;
        let mut migrated_messages = 0_u64;
        let mut source_revisions = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let actor = actor_for_role(&entry.role);
            let mut initial_relations = Vec::new();
            if let Some(previous_revision) = previous_revision.as_ref() {
                initial_relations.push(InitialRelation {
                    relation_type: "follows".to_owned(),
                    to_revision_id: previous_revision.clone(),
                });
            }
            let result = self
                .store
                .write_page(
                    WritePageRequest {
                        owner_id: self.store.owner_id().to_owned(),
                        namespace: self.scopes.conversation.clone(),
                        visibility: "private".to_owned(),
                        lifecycle_status: LifecycleStatus::Active,
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
                        initial_relations,
                        idempotency_key: Some(format!("legacy-memory:{}:{index}", entry.at)),
                    },
                    self.allowed_scopes(),
                )
                .await?;
            previous_revision = Some(result.revision_id.clone());
            source_revisions.push(result.revision_id);
            migrated_messages += u64::from(result.created);
        }
        let orientation = self
            .sync_orientation(profile, source_revisions)
            .await
            .context("migrate visible orientation into PCP")?;
        let latest_revision = self
            .recent_source_revisions(1)
            .await?
            .into_iter()
            .next()
            .or(previous_revision);
        *self.last_event_revision.lock().await = latest_revision;
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
            .write_page(
                WritePageRequest {
                    owner_id: self.store.owner_id().to_owned(),
                    namespace: self.scopes.user.clone(),
                    visibility: "private".to_owned(),
                    lifecycle_status: LifecycleStatus::Active,
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
                },
                self.allowed_scopes(),
            )
            .await?;
        let current_revision = self
            .store
            .current_revision_id(initial.page_id.clone(), self.allowed_scopes())
            .await?;
        let current = self
            .store
            .read_pages(
                ReadPagesRequest {
                    revision_ids: vec![current_revision.clone()],
                    projections: vec![Projection::Payload],
                    max_chars: 64_000,
                },
                self.allowed_scopes(),
            )
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
            .revise_page(
                RevisePageRequest {
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
                    idempotency_key: None,
                },
                self.allowed_scopes(),
            )
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
        if content.is_empty() && images.is_empty() {
            anyhow::bail!("conversation event requires text or an image");
        }
        let observed_at = now();
        let attachment_revision_ids = self
            .ingest_image_assets(&images, &observed_at)
            .await
            .context("ingest image assets into PCP")?;
        let mut parts = Vec::with_capacity(images.len() + usize::from(!content.is_empty()));
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
        let actor = actor_for_role(&role);
        let event_key = self.next_event_key();
        let mut last_event_revision = self.last_event_revision.lock().await;
        let mut initial_relations = last_event_revision
            .as_ref()
            .map(|revision_id| {
                vec![InitialRelation {
                    relation_type: "follows".to_owned(),
                    to_revision_id: revision_id.clone(),
                }]
            })
            .unwrap_or_default();
        if let Some(revision_id) = links.responds_to.as_ref() {
            initial_relations.push(InitialRelation {
                relation_type: "responds_to".to_owned(),
                to_revision_id: revision_id.clone(),
            });
        }
        initial_relations.extend(attachment_revision_ids.iter().map(|revision_id| {
            InitialRelation {
                relation_type: "has_attachment".to_owned(),
                to_revision_id: revision_id.clone(),
            }
        }));
        let mut provenance_inputs = links.input_revision_ids;
        if let Some(revision_id) = links.responds_to {
            provenance_inputs.push(revision_id);
        }
        provenance_inputs.sort();
        provenance_inputs.dedup();
        let payload_content = if entry.content.is_empty() {
            images
                .iter()
                .map(|image| format!("[Image: {}]", image.attachment.filename))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            entry.content.clone()
        };
        let tool_or_model = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.runs.last())
            .map(|run| run.model.clone());
        let page = self
            .store
            .write_page(
                WritePageRequest {
                    owner_id: self.store.owner_id().to_owned(),
                    namespace: self.scopes.conversation.clone(),
                    visibility: "private".to_owned(),
                    lifecycle_status: LifecycleStatus::Active,
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
                },
                self.allowed_scopes(),
            )
            .await?;
        entry.revision_id = Some(page.revision_id.clone());
        *last_event_revision = Some(page.revision_id.clone());
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
    ) -> Result<Vec<String>> {
        let mut revision_ids = Vec::with_capacity(images.len());
        for image in images {
            let actor = Actor {
                actor_type: ActorType::User,
                actor_id: "local-user".to_owned(),
            };
            let payload = serde_json::to_string_pretty(&image.attachment)?;
            let result = self
                .store
                .write_page(
                    WritePageRequest {
                        owner_id: self.store.owner_id().to_owned(),
                        namespace: self.scopes.conversation.clone(),
                        visibility: "private".to_owned(),
                        lifecycle_status: LifecycleStatus::Active,
                        created_by: actor.clone(),
                        observed_at: Some(observed_at.to_owned()),
                        valid_from: None,
                        valid_to: None,
                        payload: Some(PagePayload {
                            media_type: "application/vnd.symbiont.image+json".to_owned(),
                            content: payload,
                        }),
                        source_refs: vec![SourceRef {
                            source_type: "local_image".to_owned(),
                            uri: image.source_uri(),
                            locator: None,
                            metadata: None,
                        }],
                        facets: Some(json!({
                            "kind": "image_asset",
                            "sha256": image.attachment.sha256
                        })),
                        provenance: vec![ProvenanceEvent {
                            operation: "ingest".to_owned(),
                            actor,
                            timestamp: observed_at.to_owned(),
                            input_revision_ids: Vec::new(),
                            tool_or_model: None,
                        }],
                        initial_relations: Vec::new(),
                        idempotency_key: Some(format!("image-asset:{}", image.attachment.sha256)),
                    },
                    self.allowed_scopes(),
                )
                .await?;
            revision_ids.push(result.revision_id);
        }
        Ok(revision_ids)
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
        let active_assistant_revisions = pages
            .iter()
            .filter(|page| page_message_role(page) == Some(MemoryRole::Assistant))
            .map(|page| page.revision.revision_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let replied_to_revisions = pages
            .iter()
            .flat_map(|page| page.relations.iter())
            .filter(|relation| {
                relation.relation_type == "responds_to"
                    && active_assistant_revisions.contains(&relation.from_revision_id)
            })
            .map(|relation| relation.to_revision_id.clone())
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
                .is_some_and(|revision| !replied_to_revisions.contains(revision))
        {
            latest_user.delivery_state = Some(MessageDeliveryState::Failed);
        }
        Ok(entries)
    }

    pub async fn retract_latest_user_message(
        &self,
        revision_id: &str,
    ) -> Result<TombstoneCascadeResult> {
        let messages = self.recent_messages(500).await?;
        let target = messages
            .iter()
            .find(|entry| entry.revision_id.as_deref() == Some(revision_id))
            .context("message is not an active conversation event")?;
        if target.role != MemoryRole::User {
            anyhow::bail!("only user messages can be retracted");
        }
        let latest_user_revision = messages
            .iter()
            .rev()
            .find(|entry| entry.role == MemoryRole::User)
            .and_then(|entry| entry.revision_id.as_deref());
        if latest_user_revision != Some(revision_id) {
            anyhow::bail!("only the latest user message can be retracted");
        }

        let result = self
            .store
            .tombstone_derivation_cascade(
                revision_id.to_owned(),
                system_actor(),
                self.allowed_scopes(),
            )
            .await?;
        *self.last_event_revision.lock().await =
            self.recent_source_revisions(1).await?.into_iter().next();
        Ok(result)
    }

    pub async fn recent_messages_after(
        &self,
        after_revision_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let limit = limit.clamp(1, 50);
        let messages = self.recent_messages(200).await?;
        let start = after_revision_id
            .and_then(|revision_id| {
                messages
                    .iter()
                    .position(|entry| entry.revision_id.as_deref() == Some(revision_id))
            })
            .map(|index| index + 1)
            .unwrap_or_else(|| messages.len().saturating_sub(limit));
        Ok(messages.into_iter().skip(start).take(limit).collect())
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

    pub async fn read(&self, request: ReadPagesRequest) -> Result<Vec<ReadPage>> {
        self.store.read_pages(request, self.allowed_scopes()).await
    }

    pub async fn next_summary_candidate(&self, minimum_chars: usize) -> Result<Option<String>> {
        self.store
            .next_summary_candidate(self.allowed_scopes(), minimum_chars)
            .await
    }

    pub async fn mark_summary_assessed(
        &self,
        target_revision_id: String,
        outcome: &str,
        tool_or_model: Option<String>,
    ) -> Result<()> {
        self.store
            .mark_summary_assessed(
                target_revision_id,
                outcome.to_owned(),
                tool_or_model,
                self.allowed_scopes(),
            )
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
        self.store
            .write_page(
                WritePageRequest {
                    owner_id: self.store.owner_id().to_owned(),
                    namespace: namespace.to_owned(),
                    visibility: "private".to_owned(),
                    lifecycle_status: LifecycleStatus::Active,
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
                },
                self.allowed_scopes(),
            )
            .await
    }

    pub async fn write_model_summary(
        &self,
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
            .write_summary(
                WriteSummaryRequest {
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
                },
                self.allowed_scopes(),
            )
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
        provenance_inputs.extend(source_revision_ids);
        self.store
            .revise_page(
                RevisePageRequest {
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
                    idempotency_key,
                },
                self.allowed_scopes(),
            )
            .await
    }

    pub async fn link_model_pages(
        &self,
        from_revision_id: String,
        relation_type: String,
        to_revision_id: String,
        idempotency_key: Option<String>,
    ) -> Result<Relation> {
        self.store
            .link_pages(
                LinkPagesRequest {
                    from_revision_id,
                    relation_type,
                    to_revision_id,
                    created_by: model_actor(),
                    idempotency_key,
                },
                self.allowed_scopes(),
            )
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

fn system_actor() -> Actor {
    Actor {
        actor_type: ActorType::System,
        actor_id: SYSTEM_ACTOR_ID.to_owned(),
    }
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
