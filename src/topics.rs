use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    continuity::ContinuityHost,
    memory::{MemoryEntry, MessageTopicReference},
    reflection::{ConversationEpisode, ReflectionStore},
};

const MAX_TOPICS: usize = 80;
const MAX_TOPIC_MESSAGES: usize = 200;

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
        let counts = self.reflection.episode_message_counts().await?;
        Ok(TopicIndex {
            topics: topics
                .into_iter()
                .map(|topic| TopicListItem {
                    message_count: counts.get(&topic.id).copied().unwrap_or_default(),
                    topic,
                })
                .collect(),
        })
    }

    pub async fn detail(&self, id: &str) -> Result<Option<TopicDetail>> {
        let Some(topic) = self.reflection.episode(id).await? else {
            return Ok(None);
        };
        let revision_ids = self
            .reflection
            .episode_revision_ids(id, MAX_TOPIC_MESSAGES)
            .await?;
        let messages = self
            .continuity
            .messages_by_revision_ids(&revision_ids)
            .await?;
        Ok(Some(TopicDetail {
            message_count: revision_ids.len() as u64,
            topic,
            messages,
        }))
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
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use pcp_sqlite::SqlitePcpStore;

    use super::TopicService;
    use crate::{
        continuity::{ContinuityHost, MessageLinks},
        memory::MemoryRole,
        reflection::{EpisodeInput, EpisodeState, ReflectionStore},
    };

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
        let topic = reflection
            .upsert_episode(EpisodeInput {
                id: None,
                title: "Sustained topic".to_owned(),
                summary: "A compact interpretation points back to the original exchange."
                    .to_owned(),
                state: EpisodeState::Active,
                source_revision_ids: vec![user.page.revision_id.clone()],
                parent_episode_ids: Vec::new(),
            })
            .await
            .expect("create topic");
        reflection
            .attach_episode_messages(&topic.id, &[assistant.page.revision_id.clone()])
            .await
            .expect("attach assistant message");

        let service = TopicService::new(Arc::clone(&reflection), Arc::clone(&continuity));
        let index = service.index().await.expect("read topic index");
        assert_eq!(index.topics.len(), 1);
        assert_eq!(index.topics[0].message_count, 2);
        let detail = service
            .detail(&topic.id)
            .await
            .expect("read topic detail")
            .expect("topic exists");
        assert_eq!(detail.messages.len(), 2);
        assert_eq!(detail.messages[0].content, user.entry.content);
        assert_eq!(detail.messages[1].content, assistant.entry.content);
        let context = service
            .resolve_context(&topic.id)
            .await
            .expect("resolve topic context");
        assert_eq!(context.title, topic.title);

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
