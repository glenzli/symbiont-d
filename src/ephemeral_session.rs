//! RAM-only temporary discussion state.
//!
//! This owner deliberately has no persistence, PCP, inference, or web
//! dependencies. It keeps a bounded transcript until the user discards it or
//! explicitly asks another owner to persist a promotion draft.

use std::{
    collections::HashMap,
    error::Error,
    fmt::{self, Display, Formatter},
    time::{Duration, SystemTime},
};

const SESSION_ID_BYTES: usize = 16;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct EphemeralSessionId(String);

impl EphemeralSessionId {
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn random() -> Result<Self, EphemeralSessionError> {
        let mut bytes = [0_u8; SESSION_ID_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|error| EphemeralSessionError::Identifier(error.to_string()))?;
        let mut value = String::with_capacity(SESSION_ID_BYTES * 2);
        for byte in bytes {
            use fmt::Write as _;
            write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Ok(Self(value))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn from_test(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EphemeralRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EphemeralTurn {
    pub(crate) role: EphemeralRole,
    pub(crate) text: String,
    pub(crate) recorded_at: SystemTime,
    pub(crate) failure: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadOnlyMemorySeed(String);

impl ReadOnlyMemorySeed {
    pub(crate) fn new(
        text: &str,
        max_characters: usize,
    ) -> Result<Option<Self>, EphemeralSessionError> {
        if max_characters == 0 {
            return Err(EphemeralSessionError::InvalidLimits);
        }
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if text.chars().count() > max_characters {
            return Err(EphemeralSessionError::MemorySeedLimitReached);
        }
        Ok(Some(Self(text.to_owned())))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EphemeralSessionState {
    Open,
    HeldForDecision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EphemeralSessionLimits {
    pub(crate) max_turns: usize,
    pub(crate) max_characters: usize,
    pub(crate) idle_timeout: Duration,
}

impl EphemeralSessionLimits {
    #[allow(dead_code)]
    pub(crate) fn new(
        max_turns: usize,
        max_characters: usize,
        idle_timeout: Duration,
    ) -> Result<Self, EphemeralSessionError> {
        if max_turns == 0 || max_characters == 0 || idle_timeout.is_zero() {
            return Err(EphemeralSessionError::InvalidLimits);
        }
        Ok(Self {
            max_turns,
            max_characters,
            idle_timeout,
        })
    }
}

impl Default for EphemeralSessionLimits {
    fn default() -> Self {
        Self {
            max_turns: 48,
            max_characters: 120_000,
            idle_timeout: Duration::from_secs(2 * 60 * 60),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EphemeralTranscript {
    pub(crate) session_id: EphemeralSessionId,
    pub(crate) state: EphemeralSessionState,
    pub(crate) created_at: SystemTime,
    pub(crate) last_activity_at: SystemTime,
    pub(crate) turns: Vec<EphemeralTurn>,
    pub(crate) character_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EphemeralInferenceContext {
    pub(crate) session_id: EphemeralSessionId,
    pub(crate) memory_seed: Option<ReadOnlyMemorySeed>,
    pub(crate) turns: Vec<EphemeralTurn>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromotionKind {
    Conclusion,
    SelectedTurns,
    FullTranscript,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromotionSelection {
    Conclusion { markdown: String },
    SelectedTurns { indexes: Vec<usize> },
    FullTranscript,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromotionDraft {
    pub(crate) session_id: EphemeralSessionId,
    pub(crate) kind: PromotionKind,
    pub(crate) markdown: String,
    pub(crate) source_turn_indexes: Vec<usize>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum EphemeralSessionError {
    InvalidCapacity,
    InvalidLimits,
    Identifier(String),
    CapacityReached,
    NotFound,
    Expired,
    NotOpen,
    NotHeld,
    EmptyTurn,
    UnexpectedRole,
    TurnLimitReached,
    CharacterLimitReached,
    MemorySeedLimitReached,
    EmptyPromotion,
    InvalidTurnSelection,
}

impl Display for EphemeralSessionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity => {
                formatter.write_str("temporary discussion capacity must be positive")
            }
            Self::InvalidLimits => {
                formatter.write_str("temporary discussion limits must be positive")
            }
            Self::Identifier(error) => {
                write!(formatter, "create temporary discussion identifier: {error}")
            }
            Self::CapacityReached => formatter.write_str("temporary discussion capacity reached"),
            Self::NotFound => formatter.write_str("temporary discussion was not found"),
            Self::Expired => formatter.write_str("temporary discussion expired"),
            Self::NotOpen => {
                formatter.write_str("temporary discussion is waiting for a user decision")
            }
            Self::NotHeld => {
                formatter.write_str("temporary discussion must be held before promotion")
            }
            Self::EmptyTurn => formatter.write_str("temporary discussion turn cannot be empty"),
            Self::UnexpectedRole => {
                formatter.write_str("temporary discussion role order is invalid")
            }
            Self::TurnLimitReached => {
                formatter.write_str("temporary discussion turn limit reached")
            }
            Self::CharacterLimitReached => {
                formatter.write_str("temporary discussion character limit reached")
            }
            Self::MemorySeedLimitReached => {
                formatter.write_str("temporary discussion memory seed limit reached")
            }
            Self::EmptyPromotion => {
                formatter.write_str("temporary discussion promotion cannot be empty")
            }
            Self::InvalidTurnSelection => {
                formatter.write_str("temporary discussion turn selection is invalid")
            }
        }
    }
}

impl Error for EphemeralSessionError {}

#[derive(Debug)]
struct EphemeralSession {
    id: EphemeralSessionId,
    state: EphemeralSessionState,
    created_at: SystemTime,
    last_activity_at: SystemTime,
    turns: Vec<EphemeralTurn>,
    character_count: usize,
    memory_seed: Option<ReadOnlyMemorySeed>,
    limits: EphemeralSessionLimits,
}

impl EphemeralSession {
    fn is_expired(&self, now: SystemTime) -> bool {
        now.duration_since(self.last_activity_at)
            .is_ok_and(|elapsed| elapsed >= self.limits.idle_timeout)
    }

    fn snapshot(&self) -> EphemeralTranscript {
        EphemeralTranscript {
            session_id: self.id.clone(),
            state: self.state,
            created_at: self.created_at,
            last_activity_at: self.last_activity_at,
            turns: self.turns.clone(),
            character_count: self.character_count,
        }
    }

    fn append(
        &mut self,
        role: EphemeralRole,
        text: &str,
        now: SystemTime,
    ) -> Result<(), EphemeralSessionError> {
        if self.state != EphemeralSessionState::Open {
            return Err(EphemeralSessionError::NotOpen);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(EphemeralSessionError::EmptyTurn);
        }
        let expected = match self.turns.last().map(|turn| turn.role) {
            None | Some(EphemeralRole::Assistant) => EphemeralRole::User,
            Some(EphemeralRole::User) => EphemeralRole::Assistant,
        };
        if role != expected {
            return Err(EphemeralSessionError::UnexpectedRole);
        }
        if self.turns.len() >= self.limits.max_turns {
            return Err(EphemeralSessionError::TurnLimitReached);
        }
        let character_count = text.chars().count();
        if self.character_count.saturating_add(character_count) > self.limits.max_characters {
            return Err(EphemeralSessionError::CharacterLimitReached);
        }
        self.turns.push(EphemeralTurn {
            role,
            text: text.to_owned(),
            recorded_at: now,
            failure: None,
        });
        self.character_count += character_count;
        self.last_activity_at = now;
        Ok(())
    }
}

/// Process-local owner of temporary discussions.
///
/// Dropping this value, discarding a session, or expiring a session removes the
/// transcript from Symbiont's reachable state. This is lifecycle isolation,
/// not a claim that allocator memory is cryptographically erased.
pub(crate) struct EphemeralSessionStore {
    max_sessions: usize,
    sessions: HashMap<EphemeralSessionId, EphemeralSession>,
}

impl EphemeralSessionStore {
    pub(crate) fn new(max_sessions: usize) -> Result<Self, EphemeralSessionError> {
        if max_sessions == 0 {
            return Err(EphemeralSessionError::InvalidCapacity);
        }
        Ok(Self {
            max_sessions,
            sessions: HashMap::new(),
        })
    }

    pub(crate) fn start(
        &mut self,
        memory_seed: Option<ReadOnlyMemorySeed>,
        limits: EphemeralSessionLimits,
        now: SystemTime,
    ) -> Result<EphemeralSessionId, EphemeralSessionError> {
        self.expire(now);
        if self.sessions.len() >= self.max_sessions {
            return Err(EphemeralSessionError::CapacityReached);
        }
        let id = loop {
            let candidate = EphemeralSessionId::random()?;
            if !self.sessions.contains_key(&candidate) {
                break candidate;
            }
        };
        self.sessions.insert(
            id.clone(),
            EphemeralSession {
                id: id.clone(),
                state: EphemeralSessionState::Open,
                created_at: now,
                last_activity_at: now,
                turns: Vec::new(),
                character_count: 0,
                memory_seed,
                limits,
            },
        );
        Ok(id)
    }

    pub(crate) fn append_user(
        &mut self,
        id: &EphemeralSessionId,
        text: &str,
        now: SystemTime,
    ) -> Result<(), EphemeralSessionError> {
        self.live_session_mut(id, now)?
            .append(EphemeralRole::User, text, now)
    }

    pub(crate) fn append_assistant(
        &mut self,
        id: &EphemeralSessionId,
        text: &str,
        now: SystemTime,
    ) -> Result<(), EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        session.append(EphemeralRole::Assistant, text, now)?;
        if let Some(user) = session
            .turns
            .iter_mut()
            .rev()
            .find(|turn| turn.role == EphemeralRole::User)
        {
            user.failure = None;
        }
        Ok(())
    }

    /// Keeps an unanswered user turn visible and records why the host could
    /// not produce its assistant pair. The same turn can later be retried
    /// without creating a duplicate user message.
    pub(crate) fn mark_pending_user_failed(
        &mut self,
        id: &EphemeralSessionId,
        failure: &str,
        now: SystemTime,
    ) -> Result<(), EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        if session.state != EphemeralSessionState::Open {
            return Err(EphemeralSessionError::NotOpen);
        }
        let Some(turn) = session.turns.last_mut() else {
            return Err(EphemeralSessionError::UnexpectedRole);
        };
        if turn.role != EphemeralRole::User {
            return Err(EphemeralSessionError::UnexpectedRole);
        }
        let failure = failure.trim();
        turn.failure = Some(if failure.is_empty() {
            "temporary discussion reply failed".to_owned()
        } else {
            failure.chars().take(600).collect()
        });
        session.last_activity_at = now;
        Ok(())
    }

    pub(crate) fn retry_context(
        &mut self,
        id: &EphemeralSessionId,
        now: SystemTime,
    ) -> Result<EphemeralInferenceContext, EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        if session.state != EphemeralSessionState::Open {
            return Err(EphemeralSessionError::NotOpen);
        }
        let Some(turn) = session.turns.last() else {
            return Err(EphemeralSessionError::UnexpectedRole);
        };
        if turn.role != EphemeralRole::User || turn.failure.is_none() {
            return Err(EphemeralSessionError::UnexpectedRole);
        }
        Ok(EphemeralInferenceContext {
            session_id: id.clone(),
            memory_seed: session.memory_seed.clone(),
            turns: session.turns.clone(),
        })
    }

    pub(crate) fn hold_for_decision(
        &mut self,
        id: &EphemeralSessionId,
        now: SystemTime,
    ) -> Result<(), EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        if session.state != EphemeralSessionState::Open {
            return Err(EphemeralSessionError::NotOpen);
        }
        session.state = EphemeralSessionState::HeldForDecision;
        session.last_activity_at = now;
        Ok(())
    }

    pub(crate) fn resume(
        &mut self,
        id: &EphemeralSessionId,
        now: SystemTime,
    ) -> Result<(), EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        if session.state != EphemeralSessionState::HeldForDecision {
            return Err(EphemeralSessionError::NotHeld);
        }
        session.state = EphemeralSessionState::Open;
        session.last_activity_at = now;
        Ok(())
    }

    pub(crate) fn transcript(
        &mut self,
        id: &EphemeralSessionId,
        now: SystemTime,
    ) -> Result<EphemeralTranscript, EphemeralSessionError> {
        Ok(self.live_session_mut(id, now)?.snapshot())
    }

    /// Returns all state needed for one stateless inference request. The seed
    /// is read-only input and is deliberately absent from promotion drafts.
    pub(crate) fn inference_context(
        &mut self,
        id: &EphemeralSessionId,
        now: SystemTime,
    ) -> Result<EphemeralInferenceContext, EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        if session.state != EphemeralSessionState::Open {
            return Err(EphemeralSessionError::NotOpen);
        }
        Ok(EphemeralInferenceContext {
            session_id: id.clone(),
            memory_seed: session.memory_seed.clone(),
            turns: session.turns.clone(),
        })
    }

    pub(crate) fn promotion_draft(
        &mut self,
        id: &EphemeralSessionId,
        selection: PromotionSelection,
        now: SystemTime,
    ) -> Result<PromotionDraft, EphemeralSessionError> {
        let session = self.live_session_mut(id, now)?;
        if session.state != EphemeralSessionState::HeldForDecision {
            return Err(EphemeralSessionError::NotHeld);
        }
        match selection {
            PromotionSelection::Conclusion { markdown } => {
                let markdown = markdown.trim();
                if markdown.is_empty() {
                    return Err(EphemeralSessionError::EmptyPromotion);
                }
                Ok(PromotionDraft {
                    session_id: id.clone(),
                    kind: PromotionKind::Conclusion,
                    markdown: markdown.to_owned(),
                    source_turn_indexes: Vec::new(),
                })
            }
            PromotionSelection::SelectedTurns { indexes } => {
                let indexes = validate_turn_indexes(&indexes, session.turns.len())?;
                Ok(PromotionDraft {
                    session_id: id.clone(),
                    kind: PromotionKind::SelectedTurns,
                    markdown: render_turns(&session.turns, &indexes),
                    source_turn_indexes: indexes,
                })
            }
            PromotionSelection::FullTranscript => {
                if session.turns.is_empty() {
                    return Err(EphemeralSessionError::EmptyPromotion);
                }
                let indexes = (0..session.turns.len()).collect::<Vec<_>>();
                Ok(PromotionDraft {
                    session_id: id.clone(),
                    kind: PromotionKind::FullTranscript,
                    markdown: render_turns(&session.turns, &indexes),
                    source_turn_indexes: indexes,
                })
            }
        }
    }

    /// Removes a held session after another owner has durably accepted its
    /// promotion draft. This method performs no persistence itself.
    pub(crate) fn complete_promotion(
        &mut self,
        id: &EphemeralSessionId,
    ) -> Result<(), EphemeralSessionError> {
        let Some(session) = self.sessions.get(id) else {
            return Err(EphemeralSessionError::NotFound);
        };
        if session.state != EphemeralSessionState::HeldForDecision {
            return Err(EphemeralSessionError::NotHeld);
        }
        self.sessions.remove(id);
        Ok(())
    }

    pub(crate) fn discard(&mut self, id: &EphemeralSessionId) -> bool {
        self.sessions.remove(id).is_some()
    }

    pub(crate) fn expire(&mut self, now: SystemTime) -> usize {
        let previous_len = self.sessions.len();
        self.sessions.retain(|_, session| !session.is_expired(now));
        previous_len - self.sessions.len()
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.sessions.len()
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    fn live_session_mut(
        &mut self,
        id: &EphemeralSessionId,
        now: SystemTime,
    ) -> Result<&mut EphemeralSession, EphemeralSessionError> {
        let expired = self
            .sessions
            .get(id)
            .ok_or(EphemeralSessionError::NotFound)?
            .is_expired(now);
        if expired {
            self.sessions.remove(id);
            return Err(EphemeralSessionError::Expired);
        }
        self.sessions
            .get_mut(id)
            .ok_or(EphemeralSessionError::NotFound)
    }
}

fn validate_turn_indexes(
    indexes: &[usize],
    turn_count: usize,
) -> Result<Vec<usize>, EphemeralSessionError> {
    if indexes.is_empty() {
        return Err(EphemeralSessionError::InvalidTurnSelection);
    }
    let mut normalized = indexes.to_vec();
    normalized.sort_unstable();
    normalized.dedup();
    if normalized.len() != indexes.len() || normalized.iter().any(|index| *index >= turn_count) {
        return Err(EphemeralSessionError::InvalidTurnSelection);
    }
    Ok(normalized)
}

fn render_turns(turns: &[EphemeralTurn], indexes: &[usize]) -> String {
    indexes
        .iter()
        .map(|index| {
            let turn = &turns[*index];
            let role = match turn.role {
                EphemeralRole::User => "你",
                EphemeralRole::Assistant => "Symbiont",
            };
            format!("**{role}：** {}", turn.text)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
