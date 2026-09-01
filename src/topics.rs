use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration};
use serde::{Deserialize, Serialize};

use crate::{
    continuity::ContinuityHost,
    memory::{MemoryEntry, MemoryRole, MessageTopicReference},
    reflection::{ConversationEpisode, ReflectionStore},
};

const MAX_TOPICS: usize = 80;
const MAX_TOPIC_MESSAGES: usize = 200;
const TOPIC_VISIT_GAP_HOURS: i64 = 6;
const SUSTAINED_USER_TURNS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TopicAdmissionEvidence {
    pub user_turns: usize,
    pub distinct_visits: usize,
}

impl TopicAdmissionEvidence {
    pub(crate) fn from_messages<'a>(messages: impl IntoIterator<Item = &'a MemoryEntry>) -> Self {
        let mut user_times = Vec::new();
        let mut user_turns = 0;
        for message in messages {
            if message.role != MemoryRole::User {
                continue;
            }
            user_turns += 1;
            if let Ok(at) = DateTime::parse_from_rfc3339(&message.at) {
                user_times.push(at);
            }
        }
        user_times.sort();
        let mut distinct_visits = usize::from(user_turns > 0);
        for pair in user_times.windows(2) {
            if pair[1].signed_duration_since(pair[0]) > Duration::hours(TOPIC_VISIT_GAP_HOURS) {
                distinct_visits += 1;
            }
        }
        Self {
            user_turns,
            distinct_visits,
        }
    }

    pub(crate) fn qualifies(self) -> bool {
        self.user_turns >= SUSTAINED_USER_TURNS || self.user_turns >= 2 && self.distinct_visits >= 2
    }

    pub(crate) fn requirement() -> &'static str {
        "a new Topic requires either three user-authored turns or two user-authored mentions in separate conversation visits; keep one-off discussion local until it recurs"
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicContext {
    pub id: String,
    pub title: String,
    pub summary: String,
}

impl TopicContext {
    pub fn message_reference(&self) -> MessageTopicReference {
        MessageTopicReference {
            topic_id: self.id.clone(),
            title: self.title.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicListItem {
    pub topic: ConversationEpisode,
    pub message_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicIndex {
    pub topics: Vec<TopicListItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicDetail {
    pub topic: ConversationEpisode,
    pub messages: Vec<MemoryEntry>,
    pub message_count: u64,
}

pub struct TopicService {
    reflection: Arc<ReflectionStore>,
    continuity: Arc<ContinuityHost>,
}

impl TopicService {
    pub fn new(reflection: Arc<ReflectionStore>, continuity: Arc<ContinuityHost>) -> Self {
        Self {
            reflection,
            continuity,
        }
    }

    pub async fn index(&self) -> Result<TopicIndex> {
        let topics = self.reflection.episodes(MAX_TOPICS).await?;
        let topic_ids = topics
            .iter()
            .map(|topic| topic.id.clone())
            .collect::<Vec<_>>();
        let memberships = self
            .reflection
            .episode_revision_ids_by_topic(&topic_ids, MAX_TOPIC_MESSAGES)
            .await?;
        let mut member_revision_ids = memberships.values().flatten().cloned().collect::<Vec<_>>();
        member_revision_ids.sort();
        member_revision_ids.dedup();
        let mut member_messages = HashMap::new();
        for chunk in member_revision_ids.chunks(MAX_TOPIC_MESSAGES) {
            for message in self.continuity.messages_by_revision_ids(chunk).await? {
                if let Some(revision_id) = message.revision_id.clone() {
                    member_messages.insert(revision_id, message);
                }
            }
        }
        Ok(TopicIndex {
            topics: topics
                .into_iter()
                .filter(|topic| {
                    TopicAdmissionEvidence::from_messages(
                        memberships
                            .get(&topic.id)
                            .into_iter()
                            .flatten()
                            .filter_map(|revision_id| member_messages.get(revision_id)),
                    )
                    .qualifies()
                })
                .map(|topic| TopicListItem {
                    message_count: memberships
                        .get(&topic.id)
                        .map_or(0, |revision_ids| revision_ids.len() as u64),
                    topic,
                })
                .collect(),
        })
    }

    pub async fn detail(&self, id: &str) -> Result<Option<TopicDetail>> {
        let Some(topic) = self.reflection.episode(id).await? else {
            return Ok(None);
        };
        let (revision_ids, messages) = self.messages(id).await?;
        Ok(Some(TopicDetail {
            message_count: revision_ids.len() as u64,
            topic,
            messages,
        }))
    }

    pub async fn chat_history(&self, id: &str) -> Result<Vec<MemoryEntry>> {
        let (_, messages) = self.messages(id).await?;
        Ok(messages)
    }

    pub async fn latest_assistant_revision(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .chat_history(id)
            .await?
            .into_iter()
            .rev()
            .find(|message| matches!(message.role, MemoryRole::Assistant))
            .and_then(|message| message.revision_id))
    }

    pub async fn resolve_context(&self, id: &str) -> Result<TopicContext> {
        if id.len() > 128 || !id.starts_with("ep_") {
            anyhow::bail!("invalid conversation Topic ID");
        }
        let topic = self
            .reflection
            .episode(id)
            .await?
            .with_context(|| format!("conversation Topic {id} was not found"))?;
        Ok(TopicContext {
            id: topic.id,
            title: topic.title,
            summary: topic.summary,
        })
    }

    pub async fn attach_messages(&self, id: &str, revision_ids: &[String]) -> Result<()> {
        self.reflection
            .attach_episode_messages(id, revision_ids)
            .await
    }

    async fn messages(&self, id: &str) -> Result<(Vec<String>, Vec<MemoryEntry>)> {
        let revision_ids = self
            .reflection
            .episode_revision_ids(id, MAX_TOPIC_MESSAGES)
            .await?;
        let messages = self
            .continuity
            .messages_by_revision_ids(&revision_ids)
            .await?;
        Ok((revision_ids, messages))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_sqlite::SqlitePcpStore;

    use super::{TopicAdmissionEvidence, TopicService};
    use crate::{
        continuity::{ContinuityHost, MessageLinks},
        memory::{MemoryEntry, MemoryRole},
        reflection::{EpisodeInput, EpisodeState, ReflectionStore},
    };

    fn message(role: MemoryRole, at: &str) -> MemoryEntry {
        MemoryEntry {
            role,
            at: at.to_owned(),
            content: "topic evidence".to_owned(),
            revision_id: None,
            parts: Vec::new(),
            metadata: None,
            delivery_state: None,
        }
    }

    #[test]
    fn topic_admission_requires_sustained_or_revisited_user_interest() {
        let first = message(MemoryRole::User, "2026-08-01T10:00:00Z");
        let adjacent = message(MemoryRole::User, "2026-08-01T10:05:00Z");
        let sustained = message(MemoryRole::User, "2026-08-01T10:10:00Z");
        let revisited = message(MemoryRole::User, "2026-08-02T10:00:00Z");
        let assistant = message(MemoryRole::Assistant, "2026-08-01T10:06:00Z");

        assert!(
            !TopicAdmissionEvidence::from_messages([&first, &adjacent, &assistant]).qualifies()
        );
        assert!(TopicAdmissionEvidence::from_messages([&first, &adjacent, &sustained]).qualifies());
        assert!(TopicAdmissionEvidence::from_messages([&first, &revisited]).qualifies());
    }

    #[tokio::test]
    async fn reconstructs_a_topic_from_exact_conversation_revisions() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("symbiont-topics-{nonce}"));
        let pcp = Arc::new(
            SqlitePcpStore::open(root.join("context.sqlite3"))
                .await
                .expect("open PCP store"),
        );
        let continuity = Arc::new(
            ContinuityHost::open_embedded_for_test(pcp)
                .await
                .expect("open continuity"),
        );
        let reflection = Arc::new(
            ReflectionStore::open(
                root.join("reflection.sqlite3"),
                root.join("reflection.toml"),
            )
            .await
            .expect("open Reflection store"),
        );
        let user = continuity
            .ingest_message(
                MemoryRole::User,
                "A sustained line should be visible as a topic.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store user message");
        reflection
            .record_message(&user.entry, None, false, &[])
            .await
            .expect("reflect user message");
        let assistant = continuity
            .ingest_message(
                MemoryRole::Assistant,
                "The topic view should reuse this exact message.",
                Vec::new(),
                None,
                MessageLinks {
                    responds_to: Some(user.page.revision_id.clone()),
                    ..MessageLinks::default()
                },
            )
            .await
            .expect("store assistant message");
        reflection
            .record_message(&assistant.entry, Some(&user.page.revision_id), false, &[])
            .await
            .expect("reflect assistant message");
        let mut topic_sources = vec![user.page.revision_id.clone()];
        for content in [
            "The user keeps developing this same line.",
            "A third user turn makes the discussion sustained.",
        ] {
            let continuation = continuity
                .ingest_message(
                    MemoryRole::User,
                    content,
                    Vec::new(),
                    None,
                    MessageLinks::default(),
                )
                .await
                .expect("store sustained topic turn");
            reflection
                .record_message(&continuation.entry, None, false, &[])
                .await
                .expect("reflect sustained topic turn");
            topic_sources.push(continuation.page.revision_id);
        }
        continuity
            .ingest_message(
                MemoryRole::Assistant,
                "An unrelated exchange must not leak into the topic bridge.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store unrelated message");
        let topic = reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Sustained topic".to_owned(),
                summary: "A compact interpretation points back to the original exchange."
                    .to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: topic_sources,
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("create topic");
        reflection
            .attach_episode_messages(&topic.id, &[assistant.page.revision_id.clone()])
            .await
            .expect("attach assistant message");
        let incidental = continuity
            .ingest_message(
                MemoryRole::User,
                "A one-off question should stay outside the Topic sequence.",
                Vec::new(),
                None,
                MessageLinks::default(),
            )
            .await
            .expect("store incidental turn");
        reflection
            .record_message(&incidental.entry, None, false, &[])
            .await
            .expect("reflect incidental turn");
        reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Incidental line".to_owned(),
                summary: "This legacy low-evidence Episode remains internal.".to_owned(),
                state: EpisodeState::Forming,
                source_revision_ids: vec![incidental.page.revision_id],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("create legacy incidental topic");

        let service = TopicService::new(Arc::clone(&reflection), Arc::clone(&continuity));
        let index = service.index().await.expect("read topic index");
        assert_eq!(index.topics.len(), 1);
        assert_eq!(index.topics[0].topic.id, topic.id);
        assert_eq!(index.topics[0].message_count, 4);
        let detail = service
            .detail(&topic.id)
            .await
            .expect("read topic detail")
            .expect("topic exists");
        assert_eq!(detail.messages.len(), 4);
        assert_eq!(detail.messages[0].content, user.entry.content);
        assert_eq!(detail.messages[1].content, assistant.entry.content);
        let context = service
            .resolve_context(&topic.id)
            .await
            .expect("resolve topic context");
        assert_eq!(context.title, topic.title);
        let history = service
            .chat_history(&topic.id)
            .await
            .expect("read topic chat history");
        assert_eq!(history.len(), 4);
        assert!(
            history
                .iter()
                .all(|message| !message.content.contains("unrelated exchange"))
        );
        assert_eq!(
            service
                .latest_assistant_revision(&topic.id)
                .await
                .expect("read topic reply anchor"),
            Some(assistant.page.revision_id)
        );

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
