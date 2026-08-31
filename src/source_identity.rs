//! Stable identities for external sources that can safely suppress redelivery.
//!
//! A source URL is not automatically an event identity: feed pages and project
//! homepages can describe many different changes. This owner therefore only
//! recognizes source families with a sufficiently precise delivery contract.

use std::{collections::HashSet, ops::Range};

use reqwest::Url;
use sha2::{Digest, Sha256};

use crate::external_markdown::{canonical_source_url, source_urls};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecurringSectionIdentity {
    pub range: Range<usize>,
    pub identity: String,
    pub source_urls: Vec<String>,
}

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

/// Finds independently repeatable Markdown sections without treating a live
/// dashboard URL as a permanent event identity.
///
/// The URL and the normalized factual payload must both match. Calendar dates
/// in headings are ignored so an unchanged daily digest does not become a new
/// delivery merely because it was fetched again; changed scores or claims
/// still produce a different identity.
pub(crate) fn recurring_section_identities(value: &str) -> Vec<RecurringSectionIdentity> {
    let boundaries = markdown_subsection_boundaries(value);
    boundaries
        .iter()
        .enumerate()
        .filter_map(|(position, start)| {
            let end = boundaries.get(position + 1).copied().unwrap_or(value.len());
            let section = &value[*start..end];
            recurring_section_identity(section).map(|(identity, source_urls)| {
                RecurringSectionIdentity {
                    range: *start..end,
                    identity,
                    source_urls,
                }
            })
        })
        .collect()
}

fn markdown_subsection_boundaries(value: &str) -> Vec<usize> {
    let mut boundaries = Vec::new();
    let mut offset = 0;
    for line in value.split_inclusive('\n') {
        let heading = line.trim_start();
        if heading
            .strip_prefix("###")
            .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
        {
            boundaries.push(offset + line.len() - heading.len());
        }
        offset += line.len();
    }
    boundaries
}

fn recurring_section_identity(section: &str) -> Option<(String, Vec<String>)> {
    let mut urls = source_urls(section)
        .into_iter()
        .filter_map(|url| canonical_source_url(&url))
        .collect::<Vec<_>>();
    urls.sort_unstable();
    urls.dedup();
    if urls.is_empty() {
        return None;
    }

    let material = strip_iso_calendar_dates(section)
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    if material.chars().count() < 80 {
        return None;
    }
    let digest = Sha256::digest(format!("{}\n{material}", urls.join("\n")).as_bytes());
    Some((format!("section:v1:{digest:x}"), urls))
}

fn strip_iso_calendar_dates(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        if is_iso_calendar_date_at(&characters, index) {
            output.push(' ');
            index += 10;
        } else {
            output.push(characters[index]);
            index += 1;
        }
    }
    output
}

fn is_iso_calendar_date_at(characters: &[char], index: usize) -> bool {
    let Some(slice) = characters.get(index..index + 10) else {
        return false;
    };
    slice[0..4].iter().all(char::is_ascii_digit)
        && matches!(slice[4], '-' | '/')
        && slice[5..7].iter().all(char::is_ascii_digit)
        && slice[7] == slice[4]
        && slice[8..10].iter().all(char::is_ascii_digit)
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

    #[test]
    fn unchanged_dashboard_sections_share_an_identity_across_daily_dates() {
        let first = r#"### 【软件工程天梯】SWE-bench Pro 2026-08-27
* 来源：[BenchLM](https://benchlm.ai/benchmarks/swe-bench-pro)
* 结果：Claude Mythos 5 为 80.3%，Claude Fable 5 为 80.0%，Claude Opus 5 为 79.2%。"#;
        let repeated = first.replace("2026-08-27", "2026-08-31");

        assert_eq!(
            recurring_section_identities(first)[0].identity,
            recurring_section_identities(&repeated)[0].identity
        );
    }

    #[test]
    fn a_changed_dashboard_result_remains_a_new_delivery() {
        let first = r#"### 【软件工程天梯】SWE-bench Pro 2026-08-27
* 来源：[BenchLM](https://benchlm.ai/benchmarks/swe-bench-pro)
* 结果：Claude Mythos 5 为 80.3%，Claude Fable 5 为 80.0%，Claude Opus 5 为 79.2%。"#;
        let changed = first.replace("80.3%", "81.4%");

        assert_ne!(
            recurring_section_identities(first)[0].identity,
            recurring_section_identities(&changed)[0].identity
        );
    }

    #[test]
    fn plain_headings_without_attributed_urls_are_not_delivery_sections() {
        assert!(
            recurring_section_identities(
                "### 随手记录\n这一段没有来源，只是普通讨论，不应建立可跨文档抑制的身份。"
            )
            .is_empty()
        );
    }
}
