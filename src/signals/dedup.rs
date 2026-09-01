//! Compact delivery evidence outlives the UI stream, but is not durable PCP memory.
//! Section-level references prevent the first 480 characters of a mixed digest
//! from hiding a repeated claim near its end. Retrieval and verdict stay separate.

use super::{SensingDeduplicationReference, SignalDocument, SignalEvent, SignalKind};
use crate::external_digest::{DigestProvenance, ExternalDigest};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;

const HISTORY_DAYS: i64 = 180;
const MAX_REFERENCES: usize = 4096;

pub(super) fn remember(document: &mut SignalDocument, now: DateTime<Utc>) -> bool {
    let mut changed = false;
    let mut ids = document
        .delivery_history
        .iter()
        .map(|r| r.reference_id.clone())
        .collect::<HashSet<_>>();
    for signal in &document.signals {
        if signal.kind != SignalKind::ExternalInput || signal.duplicate_of_signal_id.is_some() {
            continue;
        }
        for reference in references(signal) {
            if ids.insert(reference.reference_id.clone()) {
                document.delivery_history.push(reference);
                changed = true;
            }
        }
    }
    let before = document.delivery_history.len();
    document.delivery_history.retain(|r| {
        DateTime::parse_from_rfc3339(&r.observed_at)
            .is_ok_and(|at| at.with_timezone(&Utc) >= now - Duration::days(HISTORY_DAYS))
    });
    document
        .delivery_history
        .sort_by(|a, b| a.observed_at.cmp(&b.observed_at));
    if document.delivery_history.len() > MAX_REFERENCES {
        document
            .delivery_history
            .drain(..document.delivery_history.len() - MAX_REFERENCES);
    }
    changed || before != document.delivery_history.len()
}

fn references(signal: &SignalEvent) -> Vec<SensingDeduplicationReference> {
    let source = if signal.received_text.is_empty() {
        &signal.content
    } else {
        &signal.received_text
    };
    let digest = ExternalDigest::new(
        signal.title.clone(),
        source,
        signal.event_at.clone(),
        DigestProvenance {
            fallback_url: signal
                .sources
                .first()
                .map(|s| s.url.clone())
                .unwrap_or_default(),
            source_detail: signal
                .sources
                .first()
                .map(|s| s.detail.clone())
                .unwrap_or_default(),
            possible_connection: String::new(),
        },
    );
    let Some(digest) = digest else {
        return vec![];
    };
    let sections = digest.into_candidates();
    let single = sections.len() == 1;
    sections
        .into_iter()
        .enumerate()
        .map(|(index, section)| SensingDeduplicationReference {
            reference_id: if single {
                signal.id.clone()
            } else {
                format!("{}:section:{index}", signal.id)
            },
            fingerprint: if single {
                signal.fingerprint.clone()
            } else {
                String::new()
            },
            actor_name: signal.actor.name.clone(),
            title: section.title,
            excerpt: section
                .received_text
                .unwrap_or(section.proposed_input)
                .chars()
                .take(1800)
                .collect(),
            source_urls: section.sources.into_iter().map(|s| s.url).collect(),
            event_at: signal.event_at.clone(),
            source_document_at: signal.source_document_at.clone(),
            observed_at: signal.observed_at.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivery_evidence_survives_timeline_pruning_but_expires_independently() {
        let now = Utc::now();
        let reference = SensingDeduplicationReference {
            reference_id: "delivered".into(),
            fingerprint: String::new(),
            actor_name: "Gemini".into(),
            title: "SWE-bench Pro".into(),
            excerpt: "unchanged result".into(),
            source_urls: vec![],
            event_at: None,
            source_document_at: None,
            observed_at: now.to_rfc3339(),
        };
        let mut doc = SignalDocument {
            signals: vec![],
            delivery_history: vec![reference],
        };
        remember(&mut doc, now + Duration::days(31));
        assert_eq!(doc.delivery_history.len(), 1);
        remember(&mut doc, now + Duration::days(181));
        assert!(doc.delivery_history.is_empty());
    }
}
