//! Adapts one mailbox document to the shared external-digest normalizer.
//! IMAP protocol, sender policy and cursor ownership remain in the parent.

use crate::external_digest::{DigestProvenance, ExternalDigest};
use crate::sensing::SensingCandidateDraft;

#[derive(Clone, Debug)]
pub(super) struct MailDocument {
    pub sender: String,
    pub subject: String,
    pub body: String,
    pub event_at: Option<String>,
}

impl MailDocument {
    pub(super) fn new(
        sender: String,
        subject: String,
        body: &str,
        event_at: Option<String>,
    ) -> Option<Self> {
        let digest = external_digest(&sender, &subject, body, event_at.clone())?;
        Some(Self {
            sender,
            subject,
            body: digest.body().to_owned(),
            event_at,
        })
    }

    pub(super) fn into_candidates(self) -> Vec<SensingCandidateDraft> {
        external_digest(&self.sender, &self.subject, &self.body, self.event_at)
            .expect("mail body was normalized when the document was created")
            .into_candidates()
    }

    pub(super) fn candidate_count(&self) -> usize {
        external_digest(
            &self.sender,
            &self.subject,
            &self.body,
            self.event_at.clone(),
        )
        .expect("mail body was normalized when the document was created")
        .candidate_count()
    }
}

fn external_digest(
    sender: &str,
    subject: &str,
    body: &str,
    event_at: Option<String>,
) -> Option<ExternalDigest> {
    ExternalDigest::new(
        subject.to_owned(),
        body,
        event_at,
        DigestProvenance {
            fallback_url: format!("mailto:{sender}"),
            source_detail: format!(
                "Attributed report from {sender} through the user-configured private research inbox"
            ),
            possible_connection: format!(
                "User-configured private research feed · {sender}; standalone interest is allowed and no project connection is claimed"
            ),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensing::SensingSourceClass;

    #[test]
    fn mailbox_adapter_preserves_digest_structure_and_received_text() {
        let candidates = MailDocument::new(
            "spark@example.com".to_owned(),
            "Daily digest".to_owned(),
            "Daily report 一、物理与天体 一个结果 https://example.com/a\n二、人工智能 一个发布 https://example.com/b",
            None,
        )
        .unwrap()
        .into_candidates();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].source_class, SensingSourceClass::Research);
        assert!(
            candidates[0]
                .received_text
                .as_deref()
                .is_some_and(|text| text.contains("一个结果"))
        );
    }

    #[test]
    fn mailbox_provenance_does_not_claim_a_project_connection() {
        let candidate = MailDocument::new(
            "spark@example.com".to_owned(),
            "Interesting observation".to_owned(),
            "A self-contained observation with no project relationship.",
            None,
        )
        .unwrap()
        .into_candidates()
        .remove(0);
        assert!(
            candidate
                .possible_connection
                .as_deref()
                .is_some_and(|value| value.contains("no project connection is claimed"))
        );
        assert_eq!(candidate.sources[0].url, "mailto:spark@example.com");
    }
}
