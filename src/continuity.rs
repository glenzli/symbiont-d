use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
#[cfg(test)]
use pcp_client::EmbeddedPcpClient;
use pcp_client::{DurablePageInventoryItem, PcpTenantApi};
use pcp_core::{
    AccessPrincipal, AccessPrincipalType, AccessSession, Actor, ActorType,
    AssessPageValidityRequest, CreateScopeRequest, IngestPageRequest, InitialRelation,
    LifecycleStatus, LinkPagesRequest, PageMutability, PagePayload, Projection, ProvenanceEvent,
    ReadPage, ReadPagesRequest, Relation, RevisePageRequest, Scope, SearchFilters, SearchMode,
    SearchPagesRequest, SearchResult, SourceRef, SourceSpan, ValidityStanding, WritePageRequest,
    WriteResult, WriteSummaryRequest, WriteSummaryResult, WriteValidityResult,
};
#[cfg(test)]
use pcp_store::PcpStore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    sync::{Mutex, RwLock},
};

use crate::{
    asset::{ImageAttachment, SavedImage},
    conversation_projection::ConversationProjection,
    memory::{
        MemoryEntry, MemoryRole, MemoryStore, MessageDeliveryState, MessageExternalInputReference,
        MessageMetadata, MessagePart, MessageQuote, MessageQuoteDraft, MessageTopicReference,
    },
    profile::{ProfileSnapshot, SetupStatus},
    signals::SignalEvent,
    transcript::TranscriptStore,
    working_context::{WORKING_CONTEXT_SCAN_MESSAGES, WorkingContext},
};

const USER_SCOPE_LABEL: &str = "User context";
pub const MAX_QUOTES_PER_MESSAGE: usize = 6;
pub const MAX_QUOTE_TEXT_CHARS: usize = 6_000;
pub(crate) const PROJECT_NAMESPACE: &str = "project:symbiont-d";
pub(crate) const CONVERSATION_NAMESPACE: &str = "conversation:symbiont-d-main";
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
/// One producer-local stream for all durable host events.  The sequence is
/// persisted before the ingest RPC, so retries reuse the same external event
/// identity and Runtime can namespace it as `host:symbiont-d:symbiont-main`.
const CONVERSATION_SOURCE_STREAM: &str = "symbiont-main";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct SourceSequenceState {
    next: u64,
}

struct SourceSequence {
    path: Option<PathBuf>,
    next: Mutex<u64>,
}

impl SourceSequence {
    async fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("create source sequence directory {}", parent.display())
            })?;
        }
        let next = match fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice::<SourceSequenceState>(&bytes)
                    .context("decode durable PCP source sequence")?
                    .next
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read source sequence {}", path.display()));
            }
        };
        anyhow::ensure!(next > 0, "durable PCP source sequence must start at one");
        Ok(Self {
            path: Some(path),
            next: Mutex::new(next),
        })
    }

    fn in_memory() -> Self {
        Self {
            path: None,
            next: Mutex::new(1),
        }
    }

    async fn reserve(&self) -> Result<u64> {
        let mut next = self.next.lock().await;
        let sequence = *next;
        *next = next
            .checked_add(1)
            .context("PCP source sequence exhausted")?;
        if let Some(path) = &self.path {
            let state = serde_json::to_vec(&SourceSequenceState { next: *next })?;
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, state)
                .await
                .with_context(|| format!("write source sequence {}", temporary.display()))?;
            fs::rename(&temporary, path)
                .await
                .with_context(|| format!("commit source sequence {}", path.display()))?;
        }
        Ok(sequence)
    }
}

#[derive(Clone, Debug)]
pub struct ScopePolicy {
    pub user: String,
    pub project: String,
    pub conversation: String,
}

impl ScopePolicy {
    fn for_owner(_owner_id: &str) -> Self {
        Self {
            user: "user:self".to_owned(),
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
    store: Arc<dyn PcpTenantApi>,
    transcript: Arc<TranscriptStore>,
    scopes: ScopePolicy,
    source_sequence: SourceSequence,
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
    /// Kept as `page` temporarily for call-site compatibility.  These are
    /// local transcript identifiers, not PCP Page or Revision identifiers.
    pub page: LocalMessageRecord,
    pub attachment_revision_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LocalMessageRecord {
    pub page_id: String,
    pub revision_id: String,
}

#[derive(Clone, Debug)]
pub struct MessageHistoryPage {
    pub messages: Vec<MemoryEntry>,
    pub has_more: bool,
}

struct DecodedMessageEntries {
    entries: Vec<MemoryEntry>,
    revision_page_ids: HashMap<String, String>,
    replied_to_page_ids: HashSet<String>,
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
    /// Makes a user-addressed input signal durable only after the user chooses to reply to it.
    ///
    /// The source remains an immutable external record, separate from the assistant's later
    /// interpretation and from the transient signal timeline.
    pub async fn ingest_external_signal(&self, signal: &SignalEvent) -> Result<WriteResult> {
        let observed_at = now();
        let payload = serde_json::to_string_pretty(&json!({
            "title": signal.title,
            "content": signal.content,
            "received_text": signal.received_text,
            "presentation": signal.presentation,
            "qualification_note": signal.qualification_note,
            "summary": signal.summary,
            "actor": signal.actor,
            "event_at": signal.event_at,
            "observed_at": signal.observed_at,
            "source_class": signal.source_class,
            "review_reason": signal.review_reason,
            "signal_kind": signal.kind,
            "related_signal_ids": signal.related_signal_ids,
        }))?;
        let sequence = self.source_sequence.reserve().await?;
        self.store
            .ingest_page(IngestPageRequest {
                namespace: self.scopes.conversation.clone(),
                kind: "external_signal".to_owned(),
                observed_at: Some(observed_at.clone()),
                source_span: Some(SourceSpan {
                    stream_id: CONVERSATION_SOURCE_STREAM.to_owned(),
                    start: sequence,
                    end: sequence,
                }),
                payload: Some(PagePayload {
                    media_type: "application/vnd.symbiont.external-signal+json".to_owned(),
                    content: payload,
                }),
                source_refs: signal
                    .sources
                    .iter()
                    .map(|source| SourceRef {
                        provider_id: "web".to_owned(),
                        locator: source.url.clone(),
                        media_type: Some("text/html".to_owned()),
                        content_digest: None,
                    })
                    .collect(),
                facets: Some(json!({
                    "kind": "external_signal",
                    "signal_id": signal.id,
                    "candidate_id": signal.candidate_id,
                    "source_class": signal.source_class,
                    "actor_id": signal.actor.id,
                    "actor_name": signal.actor.name,
                })),
                external_event_id: Some(format!("external-signal:{}", signal.id)),
            })
            .await
    }

    async fn external_input_references(
        &self,
        revision_ids: &[String],
    ) -> Result<Vec<MessageExternalInputReference>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut revision_ids = revision_ids.to_vec();
        revision_ids.sort();
        revision_ids.dedup();
        let pages = self
            .read(ReadPagesRequest {
                page_ids: Vec::new(),
                revision_ids,
                projections: vec![Projection::Payload, Projection::Sources, Projection::Facets],
                max_chars: 64_000,
            })
            .await?;
        Ok(pages
            .into_iter()
            .filter_map(external_input_reference_from_page)
            .collect())
    }

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

    pub async fn open(
        store: Arc<dyn PcpTenantApi>,
        transcript: Arc<TranscriptStore>,
    ) -> Result<Self> {
        Self::open_with_sequence(store, transcript, SourceSequence::in_memory()).await
    }

    pub async fn open_at(
        store: Arc<dyn PcpTenantApi>,
        transcript: Arc<TranscriptStore>,
        sequence_path: PathBuf,
    ) -> Result<Self> {
        Self::open_with_sequence(
            store,
            transcript,
            SourceSequence::open(sequence_path).await?,
        )
        .await
    }

    async fn open_with_sequence(
        store: Arc<dyn PcpTenantApi>,
        transcript: Arc<TranscriptStore>,
        source_sequence: SourceSequence,
    ) -> Result<Self> {
        let identity_id = store.identity_id().to_owned();
        let scopes = ScopePolicy::for_owner(&identity_id);
        Ok(Self {
            store,
            transcript,
            scopes,
            source_sequence,
            orientation: RwLock::new(None),
            live_conversation: ConversationProjection::new(),
        })
    }

    #[cfg(test)]
    pub async fn open_embedded_for_test(store: Arc<dyn PcpStore>) -> Result<Self> {
        let access = Self::access_session("host:symbiont-d");
        let path = std::env::temp_dir().join(format!(
            "symbiont-d-test-transcript-{}-{}.sqlite3",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let (transcript, _) = TranscriptStore::open(path, None).await?;
        Self::open(
            EmbeddedPcpClient::shared(store, access),
            Arc::new(transcript),
        )
        .await
    }

    pub fn store(&self) -> &dyn PcpTenantApi {
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
            "PCP boundary: the local transcript is authoritative for chat history. `{}` contains \
             selected durable source material for recall, not every turn and not a replayable chat \
             log. Current local message: `{current_revision}`; orientation: `{orientation}`; \
             latest checkpoint: `{checkpoint}`. Search then selectively read PCP material before \
             relying on it or asking the user to repeat it. Derived Pages may use `{}` or `{}`.",
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
        _memory: &MemoryStore,
        _profile: &ProfileSnapshot,
    ) -> Result<MigrationSummary> {
        Ok(MigrationSummary {
            migrated_messages: 0,
            orientation: None,
        })
    }

    pub async fn sync_orientation(
        &self,
        _profile: &ProfileSnapshot,
        _source_revision_ids: Vec<String>,
    ) -> Result<Option<WriteResult>> {
        // v0.8 tenant ingress is source-only.  Orientation remains local to
        // Symbiont until PCP exposes a dedicated user-owned ingest contract.
        Ok(None)
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
        // Normal conversation lives locally. Images remain available through
        // the local asset store; an explicit future capture can ingest a
        // selected source into PCP without making the transcript dependent on
        // that decision.
        let attachment_revision_ids = Vec::new();
        let external_inputs = if role == MemoryRole::User {
            self.external_input_references(&links.input_revision_ids)
                .await
                .context("resolve external input references")?
        } else {
            Vec::new()
        };
        let mut parts = Vec::with_capacity(
            images.len()
                + links.quotes.len()
                + external_inputs.len()
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
        parts.extend(
            external_inputs
                .iter()
                .cloned()
                .map(|input| MessagePart::ExternalInput { input }),
        );
        if !content.is_empty() {
            parts.push(MessagePart::Markdown {
                text: content.to_owned(),
            });
        }
        parts.extend(images.iter().map(|image| MessagePart::Image {
            asset: image.attachment.clone(),
        }));
        let entry = MemoryEntry {
            role: role.clone(),
            at: observed_at,
            content: content.to_owned(),
            revision_id: None,
            parts,
            metadata,
            delivery_state: None,
        };
        let stored = self.transcript.append(entry).await?;
        self.live_conversation.publish(stored.entry.clone()).await;
        Ok(StoredMessage {
            entry: stored.entry,
            page: LocalMessageRecord {
                page_id: stored.message_id.clone(),
                revision_id: stored.message_id,
            },
            attachment_revision_ids,
        })
    }

    async fn ingest_image_assets(
        &self,
        images: &[SavedImage],
        observed_at: &str,
        _actor: &Actor,
        _tool_or_model: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut revision_ids = Vec::with_capacity(images.len());
        for image in images {
            let payload = serde_json::to_string_pretty(&image.attachment)?;
            let sequence = self.source_sequence.reserve().await?;
            let result = self
                .store
                .ingest_page(IngestPageRequest {
                    namespace: self.scopes.conversation.clone(),
                    kind: "image_asset".to_owned(),
                    observed_at: Some(observed_at.to_owned()),
                    source_span: Some(SourceSpan {
                        stream_id: CONVERSATION_SOURCE_STREAM.to_owned(),
                        start: sequence,
                        end: sequence,
                    }),
                    payload: Some(PagePayload {
                        media_type: "application/vnd.symbiont.image+json".to_owned(),
                        content: payload,
                    }),
                    source_refs: vec![SourceRef {
                        provider_id: image.source_type().to_owned(),
                        locator: image.source_uri().to_owned(),
                        media_type: Some("application/vnd.symbiont.image+json".to_owned()),
                        content_digest: Some(image.attachment.sha256.clone()),
                    }],
                    facets: Some(json!({
                        "kind": "image_asset",
                        "sha256": image.attachment.sha256,
                        "origin": image.source_type()
                    })),
                    external_event_id: Some(format!("image-asset:{}", image.attachment.sha256)),
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
        // Quotes belong to the local transcript. PCP contains only selected
        // recall material, which may be absent or represented differently.
        let messages = self
            .transcript
            .by_ids(&revision_ids)
            .await?
            .into_iter()
            .filter_map(|entry| entry.revision_id.clone().map(|id| (id, entry)))
            .collect::<HashMap<_, _>>();

        drafts
            .into_iter()
            .map(|draft| {
                let entry = messages.get(&draft.source_revision_id).with_context(|| {
                    format!(
                        "quoted transcript message {} was not found",
                        draft.source_revision_id
                    )
                })?;
                let source_role = entry.role.clone();
                let source_at = entry.at.clone();
                let source = entry.content.clone();
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
        self.transcript.recent(limit).await
    }

    /// Reads one chronological page ending at `before_at`.
    ///
    /// PCP's temporal result order is newest first; callers receive a page in
    /// chronological order so it can be prepended to the visible transcript.
    pub async fn message_history_page(
        &self,
        before_at: Option<&str>,
        limit: usize,
    ) -> Result<MessageHistoryPage> {
        let (messages, has_more) = self.transcript.before(before_at, limit).await?;
        Ok(MessageHistoryPage { messages, has_more })
    }

    async fn message_entries_for_revisions(
        &self,
        revision_ids: &[String],
    ) -> Result<DecodedMessageEntries> {
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
        Ok(DecodedMessageEntries {
            entries,
            revision_page_ids,
            replied_to_page_ids,
        })
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
        self.transcript.by_ids(revision_ids).await
    }

    pub async fn retract_user_message_and_after(
        &self,
        revision_id: &str,
    ) -> Result<MessageRetractionResult> {
        let retraction = self.transcript.retract_from(revision_id).await?;
        self.live_conversation
            .remove(&retraction.retracted_message_ids)
            .await;
        Ok(MessageRetractionResult {
            retracted_revision_ids: retraction.retracted_message_ids.clone(),
            message_revision_ids: retraction.retracted_message_ids,
            restored_page_ids: Vec::new(),
        })
    }

    pub async fn live_messages_after(
        &self,
        after_revision_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        Ok(self.live_conversation.after(after_revision_id, limit).await)
    }

    pub fn subscribe_live_messages(&self) -> tokio::sync::watch::Receiver<u64> {
        self.live_conversation.subscribe()
    }

    pub async fn latest_assistant_revision(&self) -> Result<Option<String>> {
        Ok(self
            .transcript
            .recent(1)
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
        Ok(self
            .recent_messages(500)
            .await?
            .iter()
            .map(|entry| entry.content.chars().count())
            .sum())
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
        let page = self
            .read(ReadPagesRequest {
                page_ids: vec![page_id.to_owned()],
                revision_ids: Vec::new(),
                projections: vec![Projection::Manifest],
                max_chars: 256,
            })
            .await?
            .into_iter()
            .next()
            .context("PCP Page is not available")?;
        Ok(page.revision.revision_id)
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
        let _ = minimum_chars;
        Ok(None)
    }

    pub async fn durable_page_inventory(&self) -> Result<Vec<DurablePageInventoryItem>> {
        Ok(Vec::new())
    }

    pub async fn mark_summary_assessed(
        &self,
        _target_revision_id: String,
        _outcome: &str,
        _tool_or_model: Option<String>,
    ) -> Result<()> {
        Ok(())
    }

    pub async fn write_model_page(
        &self,
        _namespace: Option<&str>,
        _content: &str,
        _facets: Option<Value>,
        _source_refs: Vec<SourceRef>,
        _source_revision_ids: Vec<String>,
        _relations: Vec<InitialRelation>,
        _idempotency_key: Option<String>,
    ) -> Result<WriteResult> {
        anyhow::bail!("PCP v0.8 tenant mode does not permit model-authored Pages")
    }

    pub async fn write_model_summary(
        &self,
        _target_page_id: String,
        _target_revision_id: String,
        _expected_summary_revision_id: Option<String>,
        _content: String,
        _source_revision_ids: Vec<String>,
        _idempotency_key: Option<String>,
        _tool_or_model: Option<String>,
    ) -> Result<WriteSummaryResult> {
        anyhow::bail!("PCP v0.8 tenant mode does not permit model-authored summaries")
    }

    pub async fn revise_current_model_page(
        &self,
        _target_page_id: String,
        _expected_revision_id: String,
        _content: String,
        _source_revision_ids: Vec<String>,
    ) -> Result<WriteResult> {
        anyhow::bail!("PCP v0.8 tenant mode does not permit model-authored revisions")
    }

    #[cfg(any())]
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
        _target_page_id: String,
        _target_revision_id: String,
        _expected_assessment_id: Option<String>,
        _standing: ValidityStanding,
        _rationale: String,
        _scope: Option<String>,
        _basis_revision_ids: Vec<String>,
        _idempotency_key: Option<String>,
        _tool_or_model: Option<String>,
    ) -> Result<WriteValidityResult> {
        anyhow::bail!("PCP v0.8 tenant mode does not permit validity assessments")
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn revise_model_page(
        &self,
        _page_id: String,
        _expected_revision_id: String,
        _content: String,
        _facets: Option<Value>,
        _source_refs: Vec<SourceRef>,
        _lifecycle_status: LifecycleStatus,
        _source_revision_ids: Vec<String>,
        _idempotency_key: Option<String>,
    ) -> Result<WriteResult> {
        anyhow::bail!("PCP v0.8 tenant mode does not permit model-authored revisions")
    }

    pub async fn link_model_pages(
        &self,
        _from_page_id: String,
        _relation_type: String,
        _to_page_id: String,
        _basis_revision_ids: Vec<String>,
        _idempotency_key: Option<String>,
    ) -> Result<Relation> {
        anyhow::bail!("PCP v0.8 tenant mode does not permit tenant-authored relations")
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

fn external_input_reference_from_page(page: ReadPage) -> Option<MessageExternalInputReference> {
    let facets = page.revision.facets.as_ref()?;
    if page_kind(Some(facets), "") != "external_signal" {
        return None;
    }
    let payload = page.revision.payload.as_ref()?;
    if payload.media_type != "application/vnd.symbiont.external-signal+json" {
        return None;
    }
    let value = serde_json::from_str::<Value>(&payload.content).ok()?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("外部输入")
        .to_owned();
    let actor_name = facets
        .get("actor_name")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/actor/name").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("外部输入")
        .to_owned();
    let raw_excerpt = value
        .get("content")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            value
                .get("received_text")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| value.get("summary").and_then(Value::as_str))
        .unwrap_or(&title);
    let (excerpt, _) = truncate_with_flag(raw_excerpt.trim(), 360);
    let observed_at = value
        .get("observed_at")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| page.revision.observed_at.clone())
        .unwrap_or_else(|| page.revision.created_at.clone());
    Some(MessageExternalInputReference {
        source_revision_id: page.revision.revision_id,
        actor_name,
        title,
        observed_at,
        excerpt,
        source_count: page.revision.source_refs.len(),
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
        .map(|source| source.provider_id.clone());
    let revised_prompt = None;
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

fn mark_latest_user_delivery_state(
    entries: &mut [MemoryEntry],
    revision_page_ids: &HashMap<String, String>,
    replied_to_page_ids: &HashSet<String>,
) {
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
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "continuity/tests.rs"]
mod tests;
