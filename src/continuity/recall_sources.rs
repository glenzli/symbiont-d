//! Read-only, request-scoped lineage expansion. A SourceRef is a relationship,
//! never a semantic coverage verdict. Missing/unauthorized/cyclic chains stay
//! unknown; no source content is copied into PCP or persisted across ACL changes.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use pcp_core::{Projection, ReadPagesRequest, SourceRef};

use super::ContinuityHost;

const MAX_DEPTH: usize = 4;
const MAX_REVISIONS: usize = 32;

#[derive(Clone, Debug, Default)]
pub(super) struct Lineage {
    pub sources: Vec<SourceRef>,
    pub complete: bool,
}

#[derive(Default)]
struct Node {
    sources: Vec<SourceRef>,
    parents: Vec<String>,
    source_identity_complete: bool,
}

pub(super) async fn load(
    host: &ContinuityHost,
    roots: &[String],
) -> (BTreeMap<String, Lineage>, usize) {
    let mut nodes = BTreeMap::new();
    let mut requested = BTreeSet::new();
    let mut pending = roots.iter().cloned().collect::<BTreeSet<_>>();
    let mut calls = 0;
    // The timeout includes discovery and authentication. This optional metadata
    // work must not hold a foreground conversation behind a failing Runtime.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..MAX_DEPTH {
            let batch = pending
                .iter()
                .filter(|id| !requested.contains(*id))
                .take(MAX_REVISIONS.saturating_sub(requested.len()))
                .cloned()
                .collect::<Vec<_>>();
            if batch.is_empty() {
                break;
            }
            requested.extend(batch.iter().cloned());
            pending.clear();
            calls += 1;
            let Ok(pages) = host
                .read(ReadPagesRequest {
                    page_ids: Vec::new(),
                    revision_ids: batch.clone(),
                    projections: vec![
                        Projection::Manifest,
                        Projection::Sources,
                        Projection::Provenance,
                    ],
                    max_chars: 32_000,
                })
                .await
            else {
                break;
            };
            for page in pages {
                let revision = page.revision;
                if !batch.contains(&revision.revision_id) {
                    continue;
                }
                let parents = revision
                    .provenance
                    .iter()
                    .flat_map(|p| p.input_revision_ids.iter())
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                // Direct sources survive many repair operations. Walk parents
                // too: a derived Page can mix new sources with earlier Pages.
                pending.extend(parents.iter().cloned());
                let mut sources = revision.source_refs;
                let mut source_identity_complete = true;
                sources.retain(|source| {
                    let ambiguous = revision.namespace != super::PCP_NAMESPACE
                        && source.provider_id == "symbiont:transcript"
                        && source.locator.starts_with("message/");
                    source_identity_complete &= !ambiguous;
                    !ambiguous
                });
                nodes.insert(
                    revision.revision_id,
                    Node {
                        sources,
                        parents,
                        source_identity_complete,
                    },
                );
            }
        }
    })
    .await;
    (
        roots
            .iter()
            .map(|id| (id.clone(), trace(id, &nodes, &mut BTreeSet::new())))
            .collect(),
        calls,
    )
}

fn trace(id: &str, nodes: &BTreeMap<String, Node>, visiting: &mut BTreeSet<String>) -> Lineage {
    if !visiting.insert(id.to_owned()) {
        return Lineage::default();
    }
    let Some(node) = nodes.get(id) else {
        visiting.remove(id);
        return Lineage::default();
    };
    let mut result = Lineage {
        sources: node.sources.clone(),
        complete: node.source_identity_complete,
    };
    for parent in &node.parents {
        let inherited = trace(parent, nodes, visiting);
        result.complete &= inherited.complete;
        result.sources.extend(inherited.sources);
    }
    visiting.remove(id);
    let mut seen = BTreeSet::new();
    result.sources.retain(|s| {
        seen.insert((
            s.provider_id.clone(),
            s.locator.clone(),
            s.content_digest.clone(),
        ))
    });
    result
}

pub(super) fn local_id<'a>(source: &'a SourceRef, store: &str, scope: &str) -> Option<&'a str> {
    if source.provider_id != "symbiont:transcript" {
        return None;
    }
    let id = if let Some(rest) = source.locator.strip_prefix("store/") {
        let (owner, rest) = rest.split_once("/message/")?;
        if owner != store {
            return None;
        }
        rest
    } else if scope == super::PCP_NAMESPACE {
        // Historical locators have no Host identity. Never resolve those from
        // another Scope against this host's transcript by coincidental ID.
        source.locator.strip_prefix("message/")?
    } else {
        return None;
    };
    (!id.is_empty() && id.len() <= 128 && !id.contains('/')).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(locator: &str) -> SourceRef {
        SourceRef {
            provider_id: "symbiont:transcript".into(),
            locator: locator.into(),
            media_type: None,
            content_digest: None,
        }
    }

    #[test]
    fn derived_topic_follows_revisions_to_exact_sources_without_assuming_coverage() {
        let nodes = BTreeMap::from([
            (
                "topic".into(),
                Node {
                    sources: vec![],
                    parents: vec!["page".into()],
                    source_identity_complete: true,
                },
            ),
            (
                "page".into(),
                Node {
                    sources: vec![source("store/here/message/msg_1")],
                    parents: vec![],
                    source_identity_complete: true,
                },
            ),
        ]);
        let lineage = trace("topic", &nodes, &mut BTreeSet::new());
        assert!(lineage.complete);
        assert_eq!(
            local_id(&lineage.sources[0], "here", "symbiont-d"),
            Some("msg_1")
        );
        assert_eq!(local_id(&lineage.sources[0], "other", "symbiont-d"), None);
        assert_eq!(
            local_id(&source("message/msg_1"), "here", "other-scope"),
            None
        );
    }

    #[test]
    fn cycles_and_missing_sources_are_unknown_not_covered() {
        let nodes = BTreeMap::from([
            (
                "a".into(),
                Node {
                    sources: vec![],
                    parents: vec!["b".into()],
                    source_identity_complete: true,
                },
            ),
            (
                "b".into(),
                Node {
                    sources: vec![],
                    parents: vec!["a".into()],
                    source_identity_complete: true,
                },
            ),
        ]);
        assert!(!trace("a", &nodes, &mut BTreeSet::new()).complete);
        assert!(!trace("missing", &nodes, &mut BTreeSet::new()).complete);
    }
}
