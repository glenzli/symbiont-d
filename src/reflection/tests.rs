use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, SecondsFormat, Utc};

use super::{
    EpisodeInput, EpisodeState, FollowUpInput, HypothesisHorizon, HypothesisInput,
    HypothesisStatus, ReflectionStore,
};
use crate::memory::{MemoryEntry, MemoryRole};

fn temp_root(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("symbiont-reflection-{label}-{nonce}"))
}

fn message(role: MemoryRole, revision_id: &str, content: &str, at: &str) -> MemoryEntry {
    MemoryEntry {
        role,
        at: at.to_owned(),
        content: content.to_owned(),
        revision_id: Some(revision_id.to_owned()),
        parts: Vec::new(),
        metadata: None,
        delivery_state: None,
    }
}

#[tokio::test]
async fn records_interaction_facts_without_turning_them_into_scores() {
    let root = temp_root("events");
    let store = ReflectionStore::open(
        root.join("reflection.sqlite3"),
        root.join("reflection.toml"),
    )
    .await
    .unwrap();
    let assistant_at = Utc::now() - Duration::minutes(3);
    let seen_at = assistant_at + Duration::minutes(1);
    let user_at = seen_at + Duration::seconds(40);
    store
        .record_message(
            &message(
                MemoryRole::Assistant,
                "rev_assistant",
                "What changes if the conversation is asynchronous?",
                &assistant_at.to_rfc3339(),
            ),
            None,
            false,
        )
        .await
        .unwrap();
    store
        .record_seen(vec!["rev_assistant".to_owned()], seen_at.to_rfc3339())
        .await
        .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::User,
                "rev_user",
                "The delay itself is useful evidence, but not a rating.",
                &user_at.to_rfc3339(),
            ),
            Some("rev_assistant"),
            false,
        )
        .await
        .unwrap();

    let batch = store.pending_batch(20).await.unwrap().unwrap();
    let user = batch
        .events
        .iter()
        .find(|event| event.revision_id.as_deref() == Some("rev_user"))
        .unwrap();
    assert_eq!(user.payload["replyTiming"]["basis"], "seen");
    assert_eq!(user.payload["replyTiming"]["delayMs"], 40_000);
    assert!(user.payload.get("score").is_none());
    assert!(batch.source_bundle.contains("rev_user"));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn maintains_revisable_episode_and_hypothesis_projections() {
    let root = temp_root("projections");
    let store = ReflectionStore::open(
        root.join("reflection.sqlite3"),
        root.join("reflection.toml"),
    )
    .await
    .unwrap();
    let at = Utc::now().to_rfc3339();
    store
        .record_message(
            &message(
                MemoryRole::User,
                "rev_source",
                "Recent conversation should become a temporal model.",
                &at,
            ),
            None,
            false,
        )
        .await
        .unwrap();
    let episode = store
        .upsert_episode(EpisodeInput {
            id: None,
            title: "Temporal conversation model".to_owned(),
            summary: "The discussion moved from memory retrieval to interaction over time."
                .to_owned(),
            state: EpisodeState::Forming,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap();
    let revised = store
        .upsert_episode(EpisodeInput {
            id: Some(episode.id.clone()),
            title: episode.title,
            summary: "Time, response shape, and delayed follow-up are now one active design line."
                .to_owned(),
            state: EpisodeState::Active,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(revised.id, episode.id);
    assert_eq!(revised.state, EpisodeState::Active);

    store
        .upsert_hypothesis(HypothesisInput {
            id: None,
            statement: "This may be a current design priority.".to_owned(),
            evidence: "The user returned to it and expanded the architecture.".to_owned(),
            alternatives: "It may still be a temporary implementation focus.".to_owned(),
            status: HypothesisStatus::Tentative,
            horizon: HypothesisHorizon::Current,
            revisit_after: None,
            source_revision_ids: vec!["rev_source".to_owned()],
        })
        .await
        .unwrap();

    let prompt = store.prompt().await.unwrap();
    assert!(prompt.contains("Temporal conversation model"));
    assert!(prompt.contains("temporary implementation focus"));
    assert!(prompt.contains("never a rating"));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn schedules_a_candidate_follow_up_without_publishing_it() {
    let root = temp_root("follow-up");
    let store = ReflectionStore::open(
        root.join("reflection.sqlite3"),
        root.join("reflection.toml"),
    )
    .await
    .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::User,
                "rev_source",
                "Check this again after the implementation has had time to run.",
                &Utc::now().to_rfc3339(),
            ),
            None,
            false,
        )
        .await
        .unwrap();
    let not_before =
        (Utc::now() + Duration::minutes(10)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let follow_up = store
        .schedule_follow_up(FollowUpInput {
            reason: "Revisit whether the asynchronous model still changes the implementation."
                .to_owned(),
            not_before,
            source_revision_ids: vec!["rev_source".to_owned()],
        })
        .await
        .unwrap();
    assert_eq!(follow_up.status, "pending");
    assert!(store.due_follow_ups().await.unwrap().is_empty());

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn episode_parents_form_an_acyclic_auditable_graph() {
    let root = temp_root("episode-dag");
    let store = ReflectionStore::open(
        root.join("reflection.sqlite3"),
        root.join("reflection.toml"),
    )
    .await
    .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::User,
                "rev_source",
                "The conversation has moved from memory retrieval into agent runtime design.",
                &Utc::now().to_rfc3339(),
            ),
            None,
            false,
        )
        .await
        .unwrap();

    let parent = store
        .upsert_episode(EpisodeInput {
            id: None,
            title: "Memory retrieval".to_owned(),
            summary: "Earlier discussion established the retrieval boundary.".to_owned(),
            state: EpisodeState::Dormant,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap();
    let child = store
        .upsert_episode(EpisodeInput {
            id: None,
            title: "Agent runtime".to_owned(),
            summary: "The newer line continues the memory discussion at runtime scope.".to_owned(),
            state: EpisodeState::Active,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: vec![parent.id.clone()],
        })
        .await
        .unwrap();

    let cycle = store
        .upsert_episode(EpisodeInput {
            id: Some(parent.id),
            title: "Memory retrieval".to_owned(),
            summary: "A cycle must not be accepted.".to_owned(),
            state: EpisodeState::Dormant,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: vec![child.id],
        })
        .await
        .unwrap_err();
    assert!(format!("{cycle:#}").contains("cycle"));

    let unknown_source = store
        .upsert_hypothesis(HypothesisInput {
            id: None,
            statement: "This should fail.".to_owned(),
            evidence: "It cites an invented revision.".to_owned(),
            alternatives: "None.".to_owned(),
            status: HypothesisStatus::Tentative,
            horizon: HypothesisHorizon::Momentary,
            revisit_after: None,
            source_revision_ids: vec!["rev_invented".to_owned()],
        })
        .await
        .unwrap_err();
    assert!(format!("{unknown_source:#}").contains("unknown conversation Revision"));

    let _ = tokio::fs::remove_dir_all(root).await;
}
