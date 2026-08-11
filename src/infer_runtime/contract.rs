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
    fn requires_candidate_three() {
        assert_eq!(CONSUMER_PROTOCOL_VERSION, "0.1.0-candidate.3");
    }
}
