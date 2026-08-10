//! Shared normalization for bounded, attributed digest documents.
//!
//! Transport owners keep authentication, remote cursors and failure policy.
//! This module only turns one already-fetched document into independent,
//! transient sensing candidates while preserving transport-specific provenance.

use crate::external_markdown::{canonical_source_url, normalize_external_markdown};
use crate::sensing::{SensingCandidateDraft, SensingSource, SensingSourceClass};

const MAX_DOCUMENT_CHARS: usize = 24_000;
const MAX_TITLE_CHARS: usize = 240;
const MAX_SUMMARY_CHARS: usize = 1_000;
const MAX_INPUT_CHARS: usize = 1_800;
const CHINESE_SECTION_MARKERS: [&str; 10] = [
    "一、", "二、", "三、", "四、", "五、", "六、", "七、", "八、", "九、", "十、",
];

#[derive(Clone, Debug)]
pub struct DigestProvenance {
    pub fallback_url: String,
    pub source_detail: String,
    pub possible_connection: String,
}

#[derive(Clone, Debug)]
pub struct ExternalDigest {
    title: String,
    body: String,
    event_at: Option<String>,
    provenance: DigestProvenance,
}

impl ExternalDigest {
    pub fn new(
        title: String,
        body: &str,
        event_at: Option<String>,
        provenance: DigestProvenance,
    ) -> Option<Self> {
        let body = bounded_document(body);
        (!body.is_empty()).then_some(Self {
            title: compact_text(&title, MAX_TITLE_CHARS),
            body,
            event_at,
            provenance,
        })
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn into_candidates(self) -> Vec<SensingCandidateDraft> {
        let sections = split_digest(&self.body);
        let multiple_sections = sections.len() > 1;
        sections
            .into_iter()
            .map(|section| {
                let title = if multiple_sections {
                    compact_text(&section_title(&section), MAX_TITLE_CHARS)
                } else {
                    self.title.clone()
                };
                SensingCandidateDraft {
                    title,
                    summary: compact_text(&section, MAX_SUMMARY_CHARS),
                    proposed_input: compact_text(&section, MAX_INPUT_CHARS),
                    received_text: Some(normalize_external_markdown(&section)),
                    event_at: self.event_at.clone(),
                    source_class: classify_section(&section),
                    possible_connection: Some(self.provenance.possible_connection.clone()),
                    sources: extract_sources(&section, &self.title, &self.provenance),
                }
            })
            .collect()
    }

    pub fn candidate_count(&self) -> usize {
        split_digest(&self.body).len()
    }
}

fn bounded_document(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    let mut previous_blank = false;
    for line in normalized.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !previous_blank && !lines.is_empty() {
                lines.push(String::new());
            }
            previous_blank = true;
        } else {
            lines.push(line.to_owned());
            previous_blank = false;
        }
    }
    lines
        .join("\n")
        .trim()
        .chars()
        .take(MAX_DOCUMENT_CHARS)
        .collect()
}

fn split_digest(body: &str) -> Vec<String> {
    let mut boundaries = CHINESE_SECTION_MARKERS
        .iter()
        .filter_map(|marker| body.find(marker).map(|index| (index, *marker)))
        .collect::<Vec<_>>();
    boundaries.sort_unstable_by_key(|(index, _)| *index);
    if boundaries.len() >= 2 {
        return boundaries
            .iter()
            .enumerate()
            .map(|(position, (start, _))| {
                let end = boundaries
                    .get(position + 1)
                    .map(|(index, _)| *index)
                    .unwrap_or(body.len());
                body[*start..end].trim().to_owned()
            })
            .filter(|section| !section.is_empty())
            .collect();
    }

    vec![body.to_owned()]
}

fn section_title(section: &str) -> String {
    let first_line = section.lines().next().unwrap_or(section).trim();
    compact_text(first_line.trim_start_matches('#').trim(), 160)
}

fn extract_sources(body: &str, title: &str, provenance: &DigestProvenance) -> Vec<SensingSource> {
    let mut urls = Vec::new();
    for token in body.split_whitespace() {
        let token = token.trim_matches(|character: char| {
            matches!(
                character,
                '<' | '>' | '(' | ')' | '[' | ']' | ',' | '.' | ';' | '"' | '\''
            )
        });
        let Some(url) = canonical_source_url(token) else {
            continue;
        };
        if !urls.contains(&url) {
            urls.push(url);
        }
        if urls.len() == 3 {
            break;
        }
    }
    if urls.is_empty() {
        vec![SensingSource {
            url: provenance.fallback_url.clone(),
            detail: format!("{}: {title}", provenance.source_detail),
        }]
    } else {
        urls.into_iter()
            .map(|url| SensingSource {
                url,
                detail: format!("{}: {title}", provenance.source_detail),
            })
            .collect()
    }
}

fn classify_section(section: &str) -> SensingSourceClass {
    let normalized = section.to_ascii_lowercase();
    let heading = section_title(section).to_ascii_lowercase();
    if ["人工智能", "模型", "产品", "工具", "ai &", "deep learning"]
        .iter()
        .any(|term| heading.contains(term))
    {
        return SensingSourceClass::ProductsAndTools;
    }
    if ["开源", "agent", "生态", "github", "open source"]
        .iter()
        .any(|term| heading.contains(term))
    {
        return SensingSourceClass::ProjectsAndEcosystems;
    }
    if ["政策", "机构", "治理", "policy", "institution"]
        .iter()
        .any(|term| heading.contains(term))
    {
        return SensingSourceClass::InstitutionsAndPolicy;
    }
    if ["产业", "市场", "商业", "industry", "market"]
        .iter()
        .any(|term| heading.contains(term))
    {
        return SensingSourceClass::IndustryAndMarkets;
    }
    if ["文化", "书", "电影", "culture", "essay", "creative"]
        .iter()
        .any(|term| heading.contains(term))
    {
        return SensingSourceClass::CultureAndIdeas;
    }
    if [
        "研究", "物理", "天体", "科学", "论文", "arxiv", "research", "physics", "science",
    ]
    .iter()
    .any(|term| normalized.contains(term))
    {
        SensingSourceClass::Research
    } else if ["文化", "电影", "书", "culture", "essay", "creative"]
        .iter()
        .any(|term| normalized.contains(term))
    {
        SensingSourceClass::CultureAndIdeas
    } else {
        SensingSourceClass::OpenDiscovery
    }
}

fn compact_text(value: &str, limit: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(body: &str) -> ExternalDigest {
        ExternalDigest::new(
            "Daily digest".to_owned(),
            body,
            None,
            DigestProvenance {
                fallback_url: "https://example.com/digest".to_owned(),
                source_detail: "Attributed external digest".to_owned(),
                possible_connection: "No project connection is claimed".to_owned(),
            },
        )
        .unwrap()
    }

    #[test]
    fn splits_a_mixed_daily_digest_into_independent_sections() {
        let candidates = digest(
            "Daily report\n\n一、物理、天体与前沿科学\n太阳光产生量子纠缠。 https://example.com/physics\n\n二、人工智能\n一个模型发布。 https://example.com/ai",
        )
        .into_candidates();
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].title.contains("物理"));
        assert_eq!(candidates[0].source_class, SensingSourceClass::Research);
        assert!(
            candidates[0]
                .received_text
                .as_deref()
                .is_some_and(|text| text.contains("太阳光产生量子纠缠"))
        );
        assert!(candidates[1].title.contains("人工智能"));
    }

    #[test]
    fn keeps_the_first_topic_when_headings_are_flattened_or_mixed() {
        let candidates = digest(
            "[Gemini] Daily report 一、物理与天体 一个结果 https://example.com/a\n二、人工智能 一个发布 https://example.com/b\n三、开源生态 一个项目 https://example.com/c",
        )
        .into_candidates();
        assert_eq!(candidates.len(), 3);
        assert!(candidates[0].title.starts_with("一、物理"));
        assert!(candidates[1].title.starts_with("二、人工智能"));
        assert!(candidates[2].title.starts_with("三、开源生态"));
    }

    #[test]
    fn keeps_more_topics_than_one_review_batch() {
        let candidates = digest(
            "一、物理\n结果 A https://example.com/a\n二、人工智能\n结果 B https://example.com/b\n三、开源生态\n结果 C https://example.com/c\n四、论文\n结果 D https://example.com/d",
        )
        .into_candidates();
        assert_eq!(candidates.len(), 4);
        assert_eq!(
            candidates[1].source_class,
            SensingSourceClass::ProductsAndTools
        );
        assert_eq!(
            candidates[2].source_class,
            SensingSourceClass::ProjectsAndEcosystems
        );
        assert_eq!(candidates[3].source_class, SensingSourceClass::Research);
    }

    #[test]
    fn falls_back_to_the_transport_source_when_the_digest_has_no_links() {
        let candidate = digest("A self-contained observation.")
            .into_candidates()
            .remove(0);
        assert_eq!(candidate.sources[0].url, "https://example.com/digest");
        assert!(
            candidate
                .possible_connection
                .as_deref()
                .is_some_and(|value| value.contains("No project connection"))
        );
    }
}
