//! Versioned Consumer vocabulary for infer-runtime.
//!
//! Transport and product owners express semantic workloads through this
//! module instead of depending on one candidate contract's public names.

pub(crate) const CONSUMER_PROTOCOL_VERSION: &str = "0.1.0-candidate.3";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InferenceWorkload {
    SensingDuplicateClassification,
    LanguageResponse,
    DeepReasoning,
    TextSummarize,
}

impl InferenceWorkload {
    pub(crate) fn intent(self) -> &'static str {
        match self {
            Self::SensingDuplicateClassification => "text.deduplicate",
            Self::LanguageResponse => "language.respond",
            Self::DeepReasoning => "reasoning.solve",
            Self::TextSummarize => "text.summarize",
        }
    }

    /// Minimum semantic capability accepted by Symbiont for this workload.
    ///
    /// Bounded duplicate classification is local, conservative, and fails
    /// open, so a foundational model is sufficient. Text that affects visible
    /// value judgment, conversation, or durable memory remains advanced or
    /// expert.
    pub(crate) fn capability_floor(self) -> &'static str {
        match self {
            Self::SensingDuplicateClassification => "foundational",
            Self::LanguageResponse | Self::TextSummarize => "advanced",
            Self::DeepReasoning => "expert",
        }
    }

    pub(crate) fn requires_local_only(self) -> bool {
        matches!(self, Self::SensingDuplicateClassification)
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
    fn duplicate_classification_is_a_bounded_foundational_local_workload() {
        assert_eq!(
            InferenceWorkload::SensingDuplicateClassification.intent(),
            "text.deduplicate"
        );
        assert_eq!(
            InferenceWorkload::SensingDuplicateClassification.capability_floor(),
            "foundational"
        );
        assert!(InferenceWorkload::SensingDuplicateClassification.requires_local_only());
    }

    #[test]
    fn requires_candidate_three() {
        assert_eq!(CONSUMER_PROTOCOL_VERSION, "0.1.0-candidate.3");
    }
}
