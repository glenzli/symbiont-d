//! Stable identities for external sources that can safely suppress redelivery.
//!
//! A source URL is not automatically an event identity: feed pages and project
//! homepages can describe many different changes. This owner therefore only
//! recognizes source families with a sufficiently precise delivery contract.

use std::collections::HashSet;

use reqwest::Url;

use crate::external_markdown::canonical_source_url;

/// Returns an identity only when the URL names a stable, delivery-level object.
///
/// Unversioned arXiv links intentionally match other unversioned links, while
/// explicit revisions remain distinct. A later revision should therefore be
/// cited with its versioned URL when it is the reason to deliver the paper
/// again.
pub(crate) fn canonical_delivery_identity(value: &str) -> Option<String> {
    let canonical = canonical_source_url(value)?;
    let mut url = Url::parse(&canonical).ok()?;
    url.set_fragment(None);
    let retained_query = url
        .query_pairs()
        .filter(|(key, _)| !is_tracking_parameter(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !retained_query.is_empty() {
        url.query_pairs_mut().extend_pairs(retained_query);
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path().trim_end_matches('/');

    if host == "doi.org" && path.len() > 1 {
        return Some(format!("doi:{}", path.to_ascii_lowercase()));
    }
    if matches!(host.as_str(), "arxiv.org" | "www.arxiv.org") {
        let identifier = path
            .strip_prefix("/abs/")
            .or_else(|| path.strip_prefix("/pdf/"))?
            .trim_end_matches(".pdf")
            .to_ascii_lowercase();
        if identifier.is_empty() {
            return None;
        }
        return Some(match arxiv_version(&identifier) {
            Some(_) => format!("arxiv:{identifier}"),
            None => format!("arxiv:{identifier}:latest"),
        });
    }
    if host == "github.com" {
        let segments = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let exact_release =
            segments.len() >= 5 && segments[2] == "releases" && segments[3] == "tag";
        let exact_commit = segments.len() >= 4 && segments[2] == "commit";
        if exact_release || exact_commit {
            let owner = segments[0].to_ascii_lowercase();
            let repository = segments[1].to_ascii_lowercase();
            return Some(format!(
                "github:{owner}/{repository}/{}",
                segments[2..].join("/")
            ));
        }
    }
    None
}

pub(crate) fn stable_source_identities<'a>(
    urls: impl IntoIterator<Item = &'a str>,
) -> HashSet<String> {
    urls.into_iter()
        .filter_map(canonical_delivery_identity)
        .collect()
}

fn arxiv_version(identifier: &str) -> Option<u32> {
    identifier
        .rsplit_once('v')
        .and_then(|(base, version)| (!base.is_empty()).then_some(version))?
        .parse()
        .ok()
}

fn is_tracking_parameter(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "fbclid" | "gclid" | "ref" | "ref_src" | "source" | "sa" | "ust" | "ved"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unversioned_arxiv_links_have_a_stable_latest_identity() {
        assert_eq!(
            canonical_delivery_identity("https://arxiv.org/abs/2608.22152"),
            Some("arxiv:2608.22152:latest".to_owned())
        );
        assert_eq!(
            canonical_delivery_identity("https://www.arxiv.org/pdf/2608.22152.pdf"),
            Some("arxiv:2608.22152:latest".to_owned())
        );
    }

    #[test]
    fn explicit_arxiv_revisions_remain_distinct() {
        assert_ne!(
            canonical_delivery_identity("https://arxiv.org/abs/2608.22152v1"),
            canonical_delivery_identity("https://arxiv.org/abs/2608.22152v2")
        );
        assert_ne!(
            canonical_delivery_identity("https://arxiv.org/abs/2608.22152"),
            canonical_delivery_identity("https://arxiv.org/abs/2608.22152v2")
        );
    }

    #[test]
    fn homepages_and_generic_feed_urls_are_not_delivery_identities() {
        assert_eq!(canonical_delivery_identity("https://arxiv.org/"), None);
        assert_eq!(
            canonical_delivery_identity("https://example.test/research-feed"),
            None
        );
    }

    #[test]
    fn tracking_redirects_resolve_before_identity() {
        assert_eq!(
            canonical_delivery_identity(
                "https://www.google.com/url?q=https%3A%2F%2Farxiv.org%2Fabs%2F2608.22152&source=gmail"
            ),
            Some("arxiv:2608.22152:latest".to_owned())
        );
    }
}
