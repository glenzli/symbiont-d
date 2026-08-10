//! HTTP boundary for RAM-only temporary discussions.

use axum::{Json, extract::State};
use serde::Deserialize;

use crate::{
    continuity::MessageLinks,
    ephemeral_chat::{EphemeralDiscussionSnapshot, EphemeralReply},
    ephemeral_session::{PromotionKind, PromotionSelection},
    memory::{MemoryEntry, MemoryRole},
};

use super::{ApiError, AppState, ChatInterruptResponse, MAX_USER_MESSAGE_CHARS};

#[derive(Deserialize)]
pub(super) struct MessageRequest {
    message: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum PromotionRequest {
    Conclusion { markdown: String },
    SelectedTurns { indexes: Vec<usize> },
    FullTranscript,
}

pub(super) async fn temporary_discussion_snapshot(
    State(state): State<AppState>,
) -> Json<EphemeralDiscussionSnapshot> {
    Json(state.ephemeral_chat.snapshot().await)
}

pub(super) async fn reply_in_temporary_discussion(
    State(state): State<AppState>,
    Json(request): Json<MessageRequest>,
) -> Result<Json<EphemeralReply>, ApiError> {
    let message = request.message.trim();
    if message.is_empty() {
        return Err(ApiError::bad_request(
            "Temporary discussion message cannot be empty.",
        ));
    }
    if message.chars().count() > MAX_USER_MESSAGE_CHARS {
        return Err(ApiError::bad_request(format!(
            "Message exceeds {MAX_USER_MESSAGE_CHARS} characters."
        )));
    }
    if state.conversation.snapshot().await.active {
        return Err(ApiError::conflict(
            "Stop the current reply before starting a temporary discussion.",
        ));
    }
    state.conversation.announce_input();
    state.continuations.cancel_all().await;
    state
        .ephemeral_chat
        .reply(message)
        .await
        .map(Json)
        .map_err(ApiError::internal)
}

pub(super) async fn interrupt_temporary_discussion(
    State(state): State<AppState>,
) -> Json<ChatInterruptResponse> {
    Json(ChatInterruptResponse {
        accepted: state.ephemeral_chat.interrupt().await,
    })
}

pub(super) async fn hold_temporary_discussion(
    State(state): State<AppState>,
) -> Result<Json<EphemeralDiscussionSnapshot>, ApiError> {
    state
        .ephemeral_chat
        .hold()
        .await
        .map(Json)
        .map_err(ApiError::conflict)
}

pub(super) async fn resume_temporary_discussion(
    State(state): State<AppState>,
) -> Result<Json<EphemeralDiscussionSnapshot>, ApiError> {
    state
        .ephemeral_chat
        .resume()
        .await
        .map(Json)
        .map_err(ApiError::conflict)
}

pub(super) async fn discard_temporary_discussion(
    State(state): State<AppState>,
) -> Json<EphemeralDiscussionSnapshot> {
    Json(state.ephemeral_chat.discard().await)
}

pub(super) async fn promote_temporary_discussion(
    State(state): State<AppState>,
    Json(request): Json<PromotionRequest>,
) -> Result<Json<MemoryEntry>, ApiError> {
    let selection = match request {
        PromotionRequest::Conclusion { markdown } => PromotionSelection::Conclusion { markdown },
        PromotionRequest::SelectedTurns { indexes } => {
            PromotionSelection::SelectedTurns { indexes }
        }
        PromotionRequest::FullTranscript => PromotionSelection::FullTranscript,
    };
    let draft = state
        .ephemeral_chat
        .promotion_draft(selection)
        .await
        .map_err(ApiError::conflict)?;
    let heading = match draft.kind {
        PromotionKind::Conclusion => "临时讨论中保留的结论",
        PromotionKind::SelectedTurns | PromotionKind::FullTranscript => "从临时讨论保留的过程",
    };
    let content = format!("## {heading}\n\n{}", draft.markdown.trim());
    let related_revision_id = state
        .continuity
        .latest_assistant_revision()
        .await
        .map_err(ApiError::internal)?;
    let stored = state
        .continuity
        .ingest_message(
            MemoryRole::User,
            &content,
            Vec::new(),
            None,
            MessageLinks::default(),
        )
        .await
        .map_err(ApiError::internal)?;

    if let Err(error) = state.ephemeral_chat.complete_promotion().await {
        tracing::warn!(%error, "retire temporary discussion after successful promotion");
        state.ephemeral_chat.discard().await;
    }
    if let Err(error) = state
        .reflection
        .record_message(&stored.entry, related_revision_id.as_deref(), &[])
        .await
    {
        tracing::warn!(%error, "project promoted temporary discussion into reflection queue");
    }
    Ok(Json(stored.entry))
}
