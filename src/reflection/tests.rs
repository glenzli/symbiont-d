use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{Connection, params};

use super::{
    EpisodeInput, EpisodeState, FollowUpInput, HunchFeedbackTarget, HypothesisHorizon,
    HypothesisInput, HypothesisStatus, ReflectionStore,
};
use crate::memory::{MemoryEntry, MemoryRole};

fn temp_root(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("symbiont-reflection-{label}-{nonce}"))
}

fn future_review(days: i64) -> Option<String> {
    Some((Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Millis, true))
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
            &[],
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
            &[HunchFeedbackTarget {
                page_id: "pg_hunch".to_owned(),
                revision_id: "rev_hunch_pending".to_owned(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(store.pending_count().await.unwrap(), 0);
    assert!(store.pending_batch(20).await.unwrap().is_none());
    store
        .record_message(
            &message(
                MemoryRole::Assistant,
                "rev_reply",
                "Then response timing should remain weak evidence inside a complete exchange.",
                &Utc::now().to_rfc3339(),
            ),
            Some("rev_user"),
            false,
            &[],
        )
        .await
        .unwrap();

    let batch = store.pending_batch(20).await.unwrap().unwrap();
    assert!(
        batch
            .events
            .iter()
            .all(|event| event.kind != "message_seen")
    );
    let user = batch
        .events
        .iter()
        .find(|event| event.revision_id.as_deref() == Some("rev_user"))
        .unwrap();
    assert_eq!(user.payload["replyTiming"]["basis"], "seen");
    assert_eq!(user.payload["replyTiming"]["delayMs"], 40_000);
    assert!(user.payload.get("score").is_none());
    assert!(batch.source_bundle.contains("rev_user"));
    assert!(
        batch
            .source_bundle
            .contains("hunch_feedback=\"pg_hunch@rev_hunch_pending\"")
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn records_nonverbal_turn_completion_as_local_interaction_evidence() {
    let root = temp_root("turn-disposition");
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
                "rev_acknowledgement",
                "嗯，你的表述更准确。",
                &Utc::now().to_rfc3339(),
            ),
            None,
            false,
            &[],
        )
        .await
        .unwrap();
    store
        .record_turn_disposition("rev_acknowledgement", Some("👍"))
        .await
        .unwrap();

    let dispositions = store.recent_turn_dispositions(10).await.unwrap();
    assert_eq!(dispositions.len(), 1);
    assert_eq!(dispositions[0].revision_id, "rev_acknowledgement");
    assert_eq!(dispositions[0].reaction.as_deref(), Some("👍"));
    let batch = store.pending_batch(20).await.unwrap().unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.events[1].kind, "turn_reaction");
    assert!(batch.source_bundle.contains("reaction=\"👍\""));

    store
        .record_turn_disposition("rev_acknowledgement", None)
        .await
        .unwrap();
    let dispositions = store.recent_turn_dispositions(10).await.unwrap();
    assert_eq!(dispositions.len(), 1);
    assert_eq!(dispositions[0].reaction, None);

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
            &[],
        )
        .await
        .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::Assistant,
                "rev_followup",
                "The temporal model should preserve overlapping topic membership.",
                &Utc::now().to_rfc3339(),
            ),
            Some("rev_source"),
            false,
            &[],
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
    assert_eq!(
        store.episode_revision_ids(&episode.id, 20).await.unwrap(),
        vec!["rev_source".to_owned(), "rev_followup".to_owned()]
    );
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
    let mistyped_episode = store
        .upsert_episode(EpisodeInput {
            id: Some(format!("{}f", episode.id)),
            title: "Temporal conversation model".to_owned(),
            summary: "A mistyped identity must not create a duplicate.".to_owned(),
            state: EpisodeState::Active,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(format!("{mistyped_episode:#}").contains("unknown Episode ID"));
    let same_title = store
        .upsert_episode(EpisodeInput {
            id: None,
            title: " temporal conversation model ".to_owned(),
            summary: "The same title should point back to the existing Episode.".to_owned(),
            state: EpisodeState::Active,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(same_title.id, episode.id);
    store
        .attach_episode_messages(&episode.id, &["rev_followup".to_owned()])
        .await
        .unwrap();
    let overlapping = store
        .upsert_episode(EpisodeInput {
            id: None,
            title: "Overlapping thought structure".to_owned(),
            summary: "One source message may support more than one useful line of thought."
                .to_owned(),
            state: EpisodeState::Forming,
            source_revision_ids: vec!["rev_source".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        store.episode_revision_ids(&episode.id, 20).await.unwrap(),
        vec!["rev_source".to_owned(), "rev_followup".to_owned()]
    );
    assert_eq!(
        store
            .episode_revision_ids(&overlapping.id, 20)
            .await
            .unwrap(),
        vec!["rev_source".to_owned(), "rev_followup".to_owned()]
    );
    let counts = store.episode_message_counts().await.unwrap();
    assert_eq!(counts[&episode.id], 2);
    assert_eq!(counts[&overlapping.id], 2);
    store
        .record_retraction(&["rev_followup".to_owned()])
        .await
        .unwrap();
    assert_eq!(
        store.episode_revision_ids(&episode.id, 20).await.unwrap(),
        vec!["rev_source".to_owned()]
    );

    let hypothesis = store
        .upsert_hypothesis(HypothesisInput {
            id: None,
            statement: "This may be a current design priority.".to_owned(),
            evidence: "The user returned to it and expanded the architecture.".to_owned(),
            alternatives: "It may still be a temporary implementation focus.".to_owned(),
            status: HypothesisStatus::Tentative,
            horizon: HypothesisHorizon::Current,
            revisit_after: future_review(14),
            source_revision_ids: vec!["rev_source".to_owned()],
        })
        .await
        .unwrap();
    let mistyped_hypothesis = store
        .upsert_hypothesis(HypothesisInput {
            id: Some(format!("{}f", hypothesis.id)),
            statement: hypothesis.statement,
            evidence: "A typo must not fork state.".to_owned(),
            alternatives: "None.".to_owned(),
            status: HypothesisStatus::Working,
            horizon: HypothesisHorizon::Current,
            revisit_after: future_review(14),
            source_revision_ids: vec!["rev_source".to_owned()],
        })
        .await
        .unwrap_err();
    assert!(format!("{mistyped_hypothesis:#}").contains("unknown hypothesis ID"));

    let prompt = store.prompt().await.unwrap();
    assert!(prompt.contains("Temporal conversation model"));
    assert!(prompt.contains("temporary implementation focus"));
    assert!(prompt.contains("never a rating"));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn consolidates_legacy_duplicate_episode_titles_and_preserves_aliases() {
    let root = temp_root("duplicate-episodes");
    let database_path = root.join("reflection.sqlite3");
    let config_path = root.join("reflection.toml");
    let store = ReflectionStore::open(database_path.clone(), config_path.clone())
        .await
        .unwrap();
    let at = Utc::now().to_rfc3339();
    store
        .record_message(
            &message(
                MemoryRole::User,
                "rev_question",
                "How should this route?",
                &at,
            ),
            None,
            false,
            &[],
        )
        .await
        .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::Assistant,
                "rev_answer",
                "Use the lower-cost lane for reversible work.",
                &Utc::now().to_rfc3339(),
            ),
            Some("rev_question"),
            false,
            &[],
        )
        .await
        .unwrap();
    let canonical = store
        .upsert_episode(EpisodeInput {
            id: None,
            title: "Model cost and routing".to_owned(),
            summary: "The discussion established a cost-sensitive routing boundary.".to_owned(),
            state: EpisodeState::Active,
            source_revision_ids: vec!["rev_question".to_owned()],
            parent_episode_ids: Vec::new(),
        })
        .await
        .unwrap();
    drop(store);

    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute("DROP INDEX episode_normalized_title", [])
        .unwrap();
    connection
        .execute(
            "
            INSERT INTO episodes (
                id, title, summary, state, started_at, last_activity_at,
                updated_at, source_revision_ids_json, related_episode_ids_json
            ) VALUES (?1, ?2, ?3, 'active', ?4, ?4, ?4, ?5, '[]')
            ",
            params![
                "ep_legacy_duplicate",
                " model cost and routing ",
                "A legacy writer created the same topic twice.",
                at,
                serde_json::to_string(&vec!["rev_answer"]).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "
            INSERT INTO episode_messages (
                episode_id, revision_id, associated_at, association_source
            ) VALUES ('ep_legacy_duplicate', 'rev_answer', ?1, 'legacy_test')
            ",
            params![Utc::now().to_rfc3339()],
        )
        .unwrap();
    drop(connection);

    let reopened = ReflectionStore::open(database_path.clone(), config_path)
        .await
        .unwrap();
    let episodes = reopened.episodes(20).await.unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].id, canonical.id);
    assert_eq!(
        reopened.episode_aliases().await.unwrap(),
        vec![super::EpisodeAlias {
            alias_id: "ep_legacy_duplicate".to_owned(),
            canonical_id: canonical.id.clone(),
        }]
    );
    assert_eq!(
        reopened
            .episode_revision_ids("ep_legacy_duplicate", 20)
            .await
            .unwrap(),
        vec!["rev_question".to_owned(), "rev_answer".to_owned()]
    );
    assert_eq!(
        reopened
            .episode("ep_legacy_duplicate")
            .await
            .unwrap()
            .unwrap()
            .id,
        canonical.id
    );

    let connection = Connection::open(database_path).unwrap();
    let duplicate = connection.execute(
        "
        INSERT INTO episodes (
            id, title, summary, state, started_at, last_activity_at,
            updated_at, source_revision_ids_json, related_episode_ids_json
        ) VALUES ('ep_duplicate_again', 'MODEL COST AND ROUTING', 'duplicate',
                  'active', ?1, ?1, ?1, '[]', '[]')
        ",
        params![Utc::now().to_rfc3339()],
    );
    assert!(duplicate.is_err());

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
            &[],
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
    store
        .cancel_follow_ups(
            std::slice::from_ref(&follow_up.id),
            "superseded_by_continuing_user_burst",
        )
        .await
        .unwrap();
    let canceled = store.follow_ups(1).await.unwrap();
    assert_eq!(canceled[0].status, "canceled");
    assert_eq!(
        canceled[0].outcome.as_deref(),
        Some("superseded_by_continuing_user_burst")
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn marks_unfinished_reflection_runs_interrupted_after_restart() {
    let root = temp_root("restart");
    let database = root.join("reflection.sqlite3");
    let config = root.join("reflection.toml");
    let store = ReflectionStore::open(database.clone(), config.clone())
        .await
        .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::User,
                "rev_restart_user",
                "This exchange should be recoverable after a restart.",
                &Utc::now().to_rfc3339(),
            ),
            None,
            false,
            &[],
        )
        .await
        .unwrap();
    store
        .record_message(
            &message(
                MemoryRole::Assistant,
                "rev_restart_assistant",
                "The unfinished analysis must not remain running forever.",
                &Utc::now().to_rfc3339(),
            ),
            Some("rev_restart_user"),
            false,
            &[],
        )
        .await
        .unwrap();
    let batch = store.pending_batch(20).await.unwrap().unwrap();
    store.start_run("conversation", &batch).await.unwrap();
    drop(store);

    let restored = ReflectionStore::open(database, config).await.unwrap();
    let runs = restored.recent_runs(1).await.unwrap();
    assert_eq!(runs[0].status, "interrupted");
    assert_eq!(
        runs[0].error.as_deref(),
        Some("interrupted_by_service_restart")
    );
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
            &[],
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
            revisit_after: future_review(1),
            source_revision_ids: vec!["rev_invented".to_owned()],
        })
        .await
        .unwrap_err();
    assert!(format!("{unknown_source:#}").contains("unknown conversation Revision"));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn lifecycle_audit_finds_legacy_active_hypotheses_and_is_rate_limited() {
    let root = temp_root("lifecycle-audit");
    tokio::fs::create_dir_all(&root).await.unwrap();
    let database = root.join("reflection.sqlite3");
    let store = ReflectionStore::open(database.clone(), root.join("reflection.toml"))
        .await
        .unwrap();
    store
        .register_verified_revisions(&["rev_source".to_owned()])
        .await
        .unwrap();

    let missing_review = store
        .upsert_hypothesis(HypothesisInput {
            id: None,
            statement: "A new active judgment without a review date must fail.".to_owned(),
            evidence: "The source is known.".to_owned(),
            alternatives: "It may age quickly.".to_owned(),
            status: HypothesisStatus::Working,
            horizon: HypothesisHorizon::Current,
            revisit_after: None,
            source_revision_ids: vec!["rev_source".to_owned()],
        })
        .await
        .unwrap_err();
    assert!(format!("{missing_review:#}").contains("require revisit_after"));

    Connection::open(&database)
        .unwrap()
        .execute(
            "
            INSERT INTO hypotheses (
                id, statement, evidence, alternatives, status, horizon,
                revisit_after, updated_at, source_revision_ids_json
            ) VALUES (?1, ?2, ?3, ?4, 'working', 'current', NULL, ?5, ?6)
            ",
            params![
                "hyp_legacy",
                "Legacy active judgment",
                "It predates lifecycle enforcement.",
                "It may now be stale.",
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                "[\"rev_source\"]"
            ],
        )
        .unwrap();

    let health = store.projection_health().await.unwrap();
    assert_eq!(health.active_hypothesis_count, 1);
    assert_eq!(health.hypotheses_missing_revisit, 1);
    assert!(store.lifecycle_review_due().await.unwrap());
    let batch = store.lifecycle_batch().await.unwrap().unwrap();
    assert!(batch.lifecycle_audit);
    assert!(batch.events.is_empty());
    assert!(batch.source_bundle.contains("projection-lifecycle-audit"));

    store.mark_lifecycle_reviewed().await.unwrap();
    assert!(!store.lifecycle_review_due().await.unwrap());
    let retired = store
        .upsert_hypothesis(HypothesisInput {
            id: Some("hyp_legacy".to_owned()),
            statement: "Legacy active judgment".to_owned(),
            evidence: "It predates lifecycle enforcement.".to_owned(),
            alternatives: "It may now be stale.".to_owned(),
            status: HypothesisStatus::Stale,
            horizon: HypothesisHorizon::Current,
            revisit_after: None,
            source_revision_ids: vec!["rev_source".to_owned()],
        })
        .await
        .unwrap();
    assert_eq!(retired.status, HypothesisStatus::Stale);
    assert_eq!(
        store
            .projection_health()
            .await
            .unwrap()
            .active_hypothesis_count,
        0
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}
