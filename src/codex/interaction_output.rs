pub const INTERACTIVE_SETTLED_MARKER: &str = "<symbiont-settled/>";
pub const INTERACTIVE_REACTION_OPEN: &str = "<symbiont-react>";
pub const INTERACTIVE_REACTION_CLOSE: &str = "</symbiont-react>";

const MAX_REACTION_CHARS: usize = 8;
const MAX_CONTROL_OUTPUT_BYTES: usize = 96;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatDisposition {
    Reply,
    Settled,
    Reaction(String),
}

pub fn interaction_disposition_prompt() -> String {
    format!(
        "An interactive user turn does not always require an assistant message. When the user has \
         naturally completed an acknowledgement, thanks, sign-off, or conversational closure and \
         adding words would only prolong it, return exactly `{INTERACTIVE_SETTLED_MARKER}` with no \
         other text. When one lightweight emotional acknowledgement is more natural than words, \
         return exactly `{INTERACTIVE_REACTION_OPEN}👍{INTERACTIVE_REACTION_CLOSE}`, replacing 👍 \
         with one simple, context-appropriate emoji. These are deliberate interaction outcomes, \
         not errors. Use neither form for a question, request, unresolved ambiguity, material \
         correction, emotional need, risk, promised result, or useful new contribution; when in \
         doubt, reply briefly. Do not call tools or create persistent memory merely for either \
         outcome. Never describe or quote these control forms to the user."
    )
}

impl ChatDisposition {
    pub fn produces_message(&self) -> bool {
        matches!(self, Self::Reply)
    }

    pub fn reaction(&self) -> Option<&str> {
        match self {
            Self::Reaction(reaction) => Some(reaction),
            Self::Reply | Self::Settled => None,
        }
    }
}

pub fn interpret_interactive_output(text: String) -> (ChatDisposition, String) {
    match control_disposition(&text) {
        Some(disposition) => (disposition, String::new()),
        None => (ChatDisposition::Reply, text),
    }
}

pub struct InteractiveDeltaGate {
    enabled: bool,
    released: bool,
    buffered: String,
}

impl InteractiveDeltaGate {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            released: !enabled,
            buffered: String::new(),
        }
    }

    pub fn push(&mut self, delta: &str) -> Option<String> {
        if self.released {
            return Some(delta.to_owned());
        }
        self.buffered.push_str(delta);
        if could_be_control_output(&self.buffered) {
            return None;
        }
        self.released = true;
        Some(std::mem::take(&mut self.buffered))
    }

    pub fn finish(&mut self, final_text: &str) -> Option<String> {
        if self.released || !self.enabled {
            return None;
        }
        if control_disposition(final_text).is_some() {
            self.buffered.clear();
            return None;
        }
        self.released = true;
        (!self.buffered.is_empty()).then(|| std::mem::take(&mut self.buffered))
    }
}

fn control_disposition(text: &str) -> Option<ChatDisposition> {
    let trimmed = text.trim();
    if trimmed == INTERACTIVE_SETTLED_MARKER {
        return Some(ChatDisposition::Settled);
    }
    let reaction = trimmed
        .strip_prefix(INTERACTIVE_REACTION_OPEN)?
        .strip_suffix(INTERACTIVE_REACTION_CLOSE)?
        .trim();
    valid_reaction(reaction).then(|| ChatDisposition::Reaction(reaction.to_owned()))
}

fn valid_reaction(reaction: &str) -> bool {
    let chars = reaction.chars().collect::<Vec<_>>();
    !chars.is_empty()
        && chars.len() <= MAX_REACTION_CHARS
        && chars.iter().all(|character| {
            !character.is_ascii()
                && !character.is_alphanumeric()
                && !character.is_whitespace()
                && !matches!(character, '<' | '>')
        })
}

fn could_be_control_output(text: &str) -> bool {
    if text.len() > MAX_CONTROL_OUTPUT_BYTES {
        return false;
    }
    if control_disposition(text).is_some() {
        return true;
    }
    let candidate = text.trim_start();
    if candidate.is_empty() {
        return true;
    }
    if INTERACTIVE_SETTLED_MARKER.starts_with(candidate)
        || INTERACTIVE_REACTION_OPEN.starts_with(candidate)
    {
        return true;
    }
    candidate.starts_with(INTERACTIVE_REACTION_OPEN)
        && !candidate.contains(INTERACTIVE_REACTION_CLOSE)
}

#[cfg(test)]
mod tests {
    use super::{ChatDisposition, InteractiveDeltaGate, interpret_interactive_output};

    #[test]
    fn interprets_exact_settlement_and_reaction_outputs() {
        assert_eq!(
            interpret_interactive_output("  <symbiont-settled/>\n".to_owned()),
            (ChatDisposition::Settled, String::new())
        );
        assert_eq!(
            interpret_interactive_output("<symbiont-react>👍</symbiont-react>".to_owned()),
            (ChatDisposition::Reaction("👍".to_owned()), String::new())
        );
    }

    #[test]
    fn preserves_mixed_or_invalid_control_text_as_a_reply() {
        let text = "Good. <symbiont-settled/>".to_owned();
        assert_eq!(
            interpret_interactive_output(text.clone()),
            (ChatDisposition::Reply, text)
        );
        let invalid = "<symbiont-react>okay</symbiont-react>".to_owned();
        assert_eq!(
            interpret_interactive_output(invalid.clone()),
            (ChatDisposition::Reply, invalid)
        );
    }

    #[test]
    fn withholds_control_markers_without_delaying_normal_text() {
        let mut settled = InteractiveDeltaGate::new(true);
        assert_eq!(settled.push("<symbiont-"), None);
        assert_eq!(settled.push("settled/>"), None);
        assert_eq!(settled.finish("<symbiont-settled/>"), None);

        let mut reaction = InteractiveDeltaGate::new(true);
        assert_eq!(reaction.push("<symbiont-react>"), None);
        assert_eq!(reaction.push("🙂</symbiont-react>"), None);
        assert_eq!(reaction.finish("<symbiont-react>🙂</symbiont-react>"), None);

        let mut reply = InteractiveDeltaGate::new(true);
        assert_eq!(reply.push("这"), Some("这".to_owned()));
        assert_eq!(reply.push("里需要回答。"), Some("里需要回答。".to_owned()));
    }

    #[test]
    fn interaction_prompt_keeps_silence_conservative_and_explicit() {
        let prompt = super::interaction_disposition_prompt();
        assert!(prompt.contains("<symbiont-settled/>"));
        assert!(prompt.contains("<symbiont-react>👍</symbiont-react>"));
        assert!(prompt.contains("question, request"));
        assert!(prompt.contains("when in doubt, reply briefly"));
        assert!(prompt.contains("Do not call tools or create persistent memory"));
    }
}
