use std::collections::{HashSet, VecDeque};

use tokio::sync::{RwLock, watch};

use crate::memory::MemoryEntry;

const LIVE_MESSAGE_LIMIT: usize = 256;

/// Process-local replay window for messages written during the current daemon run.
///
/// Durable history remains in PCP. This projection exists only so UI heartbeats do
/// not rescan that history while checking for newly published messages.
pub struct ConversationProjection {
    messages: RwLock<VecDeque<MemoryEntry>>,
    changes: watch::Sender<u64>,
}

impl ConversationProjection {
    pub fn new() -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            messages: RwLock::new(VecDeque::with_capacity(LIVE_MESSAGE_LIMIT)),
            changes,
        }
    }

    pub async fn publish(&self, message: MemoryEntry) {
        let mut messages = self.messages.write().await;
        messages.push_back(message);
        while messages.len() > LIVE_MESSAGE_LIMIT {
            messages.pop_front();
        }
        self.notify_changed();
    }

    pub async fn after(&self, revision_id: Option<&str>, limit: usize) -> Vec<MemoryEntry> {
        let messages = self.messages.read().await;
        let start = revision_id
            .and_then(|revision_id| {
                messages
                    .iter()
                    .position(|message| message.revision_id.as_deref() == Some(revision_id))
            })
            .map(|index| index + 1)
            .unwrap_or_default();
        messages
            .iter()
            .skip(start)
            .take(limit.clamp(1, 50))
            .cloned()
            .collect()
    }

    pub async fn remove(&self, revision_ids: &[String]) {
        if revision_ids.is_empty() {
            return;
        }
        let revision_ids = revision_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut messages = self.messages.write().await;
        let before = messages.len();
        messages.retain(|message| {
            message
                .revision_id
                .as_deref()
                .is_none_or(|revision_id| !revision_ids.contains(revision_id))
        });
        if messages.len() != before {
            self.notify_changed();
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn notify_changed(&self) {
        let next = self.changes.borrow().wrapping_add(1);
        self.changes.send_replace(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryRole;

    fn message(revision_id: &str) -> MemoryEntry {
        MemoryEntry {
            role: MemoryRole::Assistant,
            at: "2026-08-04T00:00:00Z".to_owned(),
            content: revision_id.to_owned(),
            revision_id: Some(revision_id.to_owned()),
            parts: Vec::new(),
            metadata: None,
            delivery_state: None,
        }
    }

    #[tokio::test]
    async fn replays_only_messages_after_a_known_cursor() {
        let projection = ConversationProjection::new();
        projection.publish(message("rev-1")).await;
        projection.publish(message("rev-2")).await;
        projection.publish(message("rev-3")).await;

        let messages = projection.after(Some("rev-1"), 20).await;
        assert_eq!(
            messages
                .iter()
                .filter_map(|message| message.revision_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["rev-2", "rev-3"]
        );
        assert!(projection.after(Some("rev-3"), 20).await.is_empty());
    }

    #[tokio::test]
    async fn treats_an_unknown_cursor_as_a_pre_projection_baseline() {
        let projection = ConversationProjection::new();
        projection.publish(message("rev-new")).await;

        let messages = projection.after(Some("rev-before-restart"), 20).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].revision_id.as_deref(), Some("rev-new"));
    }

    #[tokio::test]
    async fn removes_retracted_messages_from_the_replay_window() {
        let projection = ConversationProjection::new();
        projection.publish(message("rev-1")).await;
        projection.publish(message("rev-2")).await;

        projection.remove(&["rev-1".to_owned()]).await;

        let messages = projection.after(None, 20).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].revision_id.as_deref(), Some("rev-2"));
    }

    #[tokio::test]
    async fn notifies_subscribers_when_the_live_projection_changes() {
        let projection = ConversationProjection::new();
        let mut updates = projection.subscribe();

        projection.publish(message("rev-1")).await;
        updates.changed().await.expect("publish notification");

        projection.remove(&["rev-1".to_owned()]).await;
        updates.changed().await.expect("retraction notification");
    }
}
