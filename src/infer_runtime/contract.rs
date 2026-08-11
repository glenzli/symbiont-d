//! Versioned Consumer vocabulary for infer-runtime.
//!
//! Transport and product owners express semantic workloads through this
//! module instead of depending on one candidate contract's public names.

pub(crate) const CONSUMER_PROTOCOL_VERSION: &str = "0.1.0-candidate.3";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InferenceWorkload {
    LanguageResponse,
    DeepReasoning,
    TextSummarize,
}

impl InferenceWorkload {
    pub(crate) fn intent(self) -> &'static str {
        match self {
            Self::LanguageResponse => "language.respond",
            Self::DeepReasoning => "reasoning.solve",
            Self::TextSummarize => "text.summarize",
        }
    }

    /// Minimum semantic capability accepted by Symbiont for this workload.
    ///
    /// Bounded mechanical work such as audio transcription has its own
    /// contract. Every text workload here can affect conversation, attention,
    /// or durable memory structure and therefore must not fall back to a
    /// merely capable model.
    pub(crate) fn capability_floor(self) -> &'static str {
        match self {
            Self::LanguageResponse | Self::TextSummarize => "advanced",
            Self::DeepReasoning => "expert",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_semantic_workloads_to_candidate_three_vocabulary() {
        assert_eq!(
            InferenceWorkload::LanguageResponse.intent(),
            "language.respond"
        );
        assert_eq!(InferenceWorkload::DeepReasoning.intent(), "reasoning.solve");
        assert_eq!(InferenceWorkload::TextSummarize.intent(), "text.summarize");
    }

    #[test]
    fn semantic_workloads_never_accept_merely_capable_models() {
        assert_eq!(
            InferenceWorkload::LanguageResponse.capability_floor(),
            "advanced"
        );
        assert_eq!(
            InferenceWorkload::TextSummarize.capability_floor(),
            "advanced"
        );
        assert_eq!(
            InferenceWorkload::DeepReasoning.capability_floor(),
            "expert"
        );
    }

    #[test]
    fn requires_candidate_three() {
        assert_eq!(CONSUMER_PROTOCOL_VERSION, "0.1.0-candidate.3");
    }
}
