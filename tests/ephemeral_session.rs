#[path = "../src/ephemeral_session.rs"]
mod ephemeral_session;

use std::time::{Duration, SystemTime};

use ephemeral_session::{
    EphemeralRole, EphemeralSessionError, EphemeralSessionLimits, EphemeralSessionState,
    EphemeralSessionStore, PromotionKind, PromotionSelection, ReadOnlyMemorySeed,
};

fn limits() -> EphemeralSessionLimits {
    EphemeralSessionLimits::new(4, 64, Duration::from_secs(30)).unwrap()
}

#[test]
fn keeps_a_bounded_transcript_only_in_the_process_store() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let mut store = EphemeralSessionStore::new(2).unwrap();
    let seed = ReadOnlyMemorySeed::new("既有记忆只读摘要", 64).unwrap();
    let id = store.start(seed, limits(), start).unwrap();

    store.append_user(&id, "讨论一个暂时的问题", start).unwrap();
    store
        .append_assistant(&id, "可以，这段不会自动进入记忆。", start)
        .unwrap();

    let transcript = store.transcript(&id, start).unwrap();
    assert_eq!(transcript.session_id, id);
    assert_eq!(transcript.state, EphemeralSessionState::Open);
    assert_eq!(transcript.created_at, start);
    assert_eq!(transcript.last_activity_at, start);
    assert_eq!(transcript.turns.len(), 2);
    assert_eq!(transcript.turns[0].role, EphemeralRole::User);
    assert_eq!(transcript.turns[1].role, EphemeralRole::Assistant);
    assert_eq!(transcript.character_count, 23);
    assert!(
        id.as_str()
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );

    let context = store.inference_context(&id, start).unwrap();
    assert_eq!(context.session_id, id);
    assert_eq!(
        context.memory_seed.as_ref().map(ReadOnlyMemorySeed::as_str),
        Some("既有记忆只读摘要")
    );
    assert_eq!(context.turns, transcript.turns);

    assert!(store.discard(&id));
    assert!(store.is_empty());
}

#[test]
fn rejects_invalid_order_and_limits_without_partial_append() {
    let now = SystemTime::UNIX_EPOCH;
    let limits = EphemeralSessionLimits::new(2, 5, Duration::from_secs(30)).unwrap();
    let mut store = EphemeralSessionStore::new(1).unwrap();
    let id = store.start(None, limits, now).unwrap();

    assert_eq!(
        store.append_assistant(&id, "先回答", now),
        Err(EphemeralSessionError::UnexpectedRole)
    );
    store.append_user(&id, "12345", now).unwrap();
    assert_eq!(
        store.append_assistant(&id, "6", now),
        Err(EphemeralSessionError::CharacterLimitReached)
    );
    let transcript = store.transcript(&id, now).unwrap();
    assert_eq!(transcript.turns.len(), 1);
    assert_eq!(transcript.character_count, 5);
}

#[test]
fn hold_blocks_new_turns_until_the_user_resumes() {
    let now = SystemTime::UNIX_EPOCH;
    let later = now + Duration::from_secs(1);
    let mut store = EphemeralSessionStore::new(1).unwrap();
    let id = store.start(None, limits(), now).unwrap();
    store.append_user(&id, "先聊聊", now).unwrap();
    store.append_assistant(&id, "好", now).unwrap();

    store.hold_for_decision(&id, later).unwrap();
    assert_eq!(
        store.append_user(&id, "再说一句", later),
        Err(EphemeralSessionError::NotOpen)
    );
    store.resume(&id, later).unwrap();
    store.append_user(&id, "再说一句", later).unwrap();
}

#[test]
fn failed_inference_keeps_the_pending_user_turn_for_retry() {
    let now = SystemTime::UNIX_EPOCH;
    let mut store = EphemeralSessionStore::new(1).unwrap();
    let id = store.start(None, limits(), now).unwrap();
    store.append_user(&id, "写错的问题", now).unwrap();

    store
        .mark_pending_user_failed(&id, "runtime unavailable", now)
        .unwrap();
    let transcript = store.transcript(&id, now).unwrap();
    assert_eq!(transcript.turns.len(), 1);
    assert_eq!(transcript.turns[0].text, "写错的问题");
    assert_eq!(
        transcript.turns[0].failure.as_deref(),
        Some("runtime unavailable")
    );
    assert_eq!(transcript.character_count, 5);

    let retry = store.retry_context(&id, now).unwrap();
    assert_eq!(retry.turns.len(), 1);
    assert_eq!(retry.turns[0].text, "写错的问题");
    store.append_assistant(&id, "新的回答", now).unwrap();
    assert_eq!(
        store.retry_context(&id, now),
        Err(EphemeralSessionError::UnexpectedRole)
    );
    assert_eq!(store.transcript(&id, now).unwrap().turns[0].failure, None);
}

#[test]
fn promotion_is_an_explicit_draft_and_does_not_retire_the_session() {
    let now = SystemTime::UNIX_EPOCH;
    let mut store = EphemeralSessionStore::new(1).unwrap();
    let seed = ReadOnlyMemorySeed::new("不要把这段既有记忆复制进转入草稿", 64).unwrap();
    let id = store.start(seed, limits(), now).unwrap();
    store.append_user(&id, "原问题", now).unwrap();
    store.append_assistant(&id, "临时回答", now).unwrap();

    assert_eq!(
        store.promotion_draft(
            &id,
            PromotionSelection::Conclusion {
                markdown: "结论".into()
            },
            now
        ),
        Err(EphemeralSessionError::NotHeld)
    );
    store.hold_for_decision(&id, now).unwrap();

    let conclusion = store
        .promotion_draft(
            &id,
            PromotionSelection::Conclusion {
                markdown: "  用户确认的结论  ".into(),
            },
            now,
        )
        .unwrap();
    assert_eq!(conclusion.kind, PromotionKind::Conclusion);
    assert_eq!(conclusion.markdown, "用户确认的结论");
    assert!(!conclusion.markdown.contains("既有记忆"));
    assert!(conclusion.source_turn_indexes.is_empty());
    assert_eq!(store.len(), 1);

    let selected = store
        .promotion_draft(
            &id,
            PromotionSelection::SelectedTurns {
                indexes: vec![1, 0],
            },
            now,
        )
        .unwrap();
    assert_eq!(selected.kind, PromotionKind::SelectedTurns);
    assert_eq!(selected.source_turn_indexes, vec![0, 1]);
    assert_eq!(
        selected.markdown,
        "**你：** 原问题\n\n**Symbiont：** 临时回答"
    );
    assert_eq!(selected.session_id, id);

    let full = store
        .promotion_draft(&id, PromotionSelection::FullTranscript, now)
        .unwrap();
    assert_eq!(full.kind, PromotionKind::FullTranscript);
    assert_eq!(store.len(), 1);

    store.complete_promotion(&id).unwrap();
    assert!(store.is_empty());
}

#[test]
fn expiry_and_capacity_remove_reachable_transcripts() {
    let now = SystemTime::UNIX_EPOCH;
    let expiry = now + Duration::from_secs(30);
    let mut store = EphemeralSessionStore::new(1).unwrap();
    let id = store.start(None, limits(), now).unwrap();
    store.append_user(&id, "临时内容", now).unwrap();

    assert_eq!(
        store.start(None, limits(), now),
        Err(EphemeralSessionError::CapacityReached)
    );
    assert_eq!(store.expire(expiry), 1);
    assert_eq!(
        store.transcript(&id, expiry),
        Err(EphemeralSessionError::NotFound)
    );
    assert!(store.start(None, limits(), expiry).is_ok());
}

#[test]
fn read_only_memory_seed_is_bounded_before_a_session_starts() {
    assert_eq!(
        ReadOnlyMemorySeed::new("123456", 5),
        Err(EphemeralSessionError::MemorySeedLimitReached)
    );
    assert_eq!(ReadOnlyMemorySeed::new("   ", 5).unwrap(), None);
}
