//! Turns one bounded, attributed mailbox document into a few independent
//! sensing candidates. IMAP protocol and cursor policy remain in the parent
//! module; digest structure and source normalization live here.

use reqwest::Url;

use crate::sensing::{SensingCandidateDraft, SensingSource, SensingSourceClass};

const MAX_DOCUMENT_CHARS: usize = 24_000;
const MAX_TITLE_CHARS: usize = 240;
const MAX_SUMMARY_CHARS: usize = 1_000;
const MAX_INPUT_CHARS: usize = 1_800;
const CHINESE_SECTION_MARKERS: [&str; 10] = [
    "一、", "二、", "三、", "四、", "五、", "六、", "七、", "八、", "九、", "十、",
];

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
        let body = bounded_document(body);
        (!body.is_empty()).then_some(Self {
            sender,
            subject,
            body,
            event_at,
        })
    }

    pub(super) fn into_candidates(self) -> Vec<SensingCandidateDraft> {
        let sections = split_digest(&self.body);
        let multiple_sections = sections.len() > 1;
        sections
            .into_iter()
            .map(|section| {
                let section_title = section_title(&section);
                let title = if multiple_sections {
                    compact_text(&section_title, MAX_TITLE_CHARS)
                } else {
                    self.subject.clone()
                };
                SensingCandidateDraft {
                    title,
                    summary: compact_text(&section, MAX_SUMMARY_CHARS),
                    proposed_input: compact_text(&section, MAX_INPUT_CHARS),
                    event_at: self.event_at.clone(),
                    source_class: classify_section(&section),
                    possible_connection: Some(format!(
                        "User-configured private research feed · {}; standalone interest is allowed and no project connection is claimed",
                        self.sender
                    )),
                    sources: extract_sources(&section, &self.sender, &self.subject),
                }
            })
            .collect()
    }

    pub(super) fn candidate_count(&self) -> usize {
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
    // Digest generators and MIME-to-text conversion do not agree on heading
    // line breaks: the first marker is often attached to the subject/preamble
    // while later markers start their own lines. Detect the bounded structural
    // markers in the complete document so that choosing a line-oriented path
    // cannot silently drop the first topic.
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

fn extract_sources(body: &str, sender: &str, subject: &str) -> Vec<SensingSource> {
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
            url: format!("mailto:{sender}"),
            detail: format!(
                "Attributed report from the user-configured private research inbox: {subject}"
            ),
        }]
    } else {
        urls.into_iter()
            .map(|url| SensingSource {
                url,
                detail: format!(
                    "Linked by {sender} through the user-configured private research inbox: {subject}"
                ),
            })
            .collect()
    }
}

fn canonical_source_url(value: &str) -> Option<String> {
    let parsed = Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let google_redirect = parsed
        .host_str()
        .is_some_and(|host| host == "google.com" || host.ends_with(".google.com"))
        && parsed.path() == "/url";
    if google_redirect {
        for (key, value) in parsed.query_pairs() {
            if matches!(key.as_ref(), "q" | "url") {
                if let Ok(target) = Url::parse(&value) {
                    if matches!(target.scheme(), "http" | "https") {
                        return Some(target.to_string());
                    }
                }
            }
        }
    }
    Some(parsed.to_string())
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

    #[test]
    fn splits_a_mixed_daily_digest_into_independent_broad_sections() {
        let document = MailDocument::new(
            "spark@example.com".to_owned(),
            "Daily digest".to_owned(),
            "Daily report\n\n一、物理、天体与前沿科学\n太阳光产生量子纠缠。 https://example.com/physics\n\n二、人工智能\n一个模型发布。 https://example.com/ai",
            None,
        )
        .unwrap();
        let candidates = document.into_candidates();
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].title.contains("物理"));
        assert_eq!(candidates[0].source_class, SensingSourceClass::Research);
        assert!(candidates[1].title.contains("人工智能"));
    }

    #[test]
    fn splits_flattened_chinese_digest_sections() {
        let sections = split_digest(
            "Daily report 一、物理与天体 一个结果 https://example.com/a 二、人工智能 一个发布 https://example.com/b",
        );
        assert_eq!(sections.len(), 2);
        assert!(sections[0].starts_with("一、"));
        assert!(sections[1].starts_with("二、"));
    }

    #[test]
    fn keeps_the_first_topic_when_only_later_headings_start_new_lines() {
        let sections = split_digest(
            "[Gemini] Daily report 一、物理与天体 一个结果 https://example.com/a\n二、人工智能 一个发布 https://example.com/b\n三、开源生态 一个项目 https://example.com/c",
        );
        assert_eq!(sections.len(), 3);
        assert!(sections[0].starts_with("一、物理"));
        assert!(sections[1].starts_with("二、人工智能"));
        assert!(sections[2].starts_with("三、开源生态"));
    }

    #[test]
    fn keeps_more_topics_than_one_review_batch_for_the_transient_queue() {
        let document = MailDocument::new(
            "spark@example.com".to_owned(),
            "Daily digest".to_owned(),
            "一、物理\n结果 A https://example.com/a\n二、人工智能\n结果 B https://example.com/b\n三、开源生态\n结果 C https://example.com/c\n四、论文\n结果 D https://example.com/d",
            None,
        )
        .unwrap();

        let candidates = document.into_candidates();
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
    fn unwraps_google_redirects_to_the_actual_source() {
        assert_eq!(
            canonical_source_url(
                "https://www.google.com/url?q=https%3A%2F%2Fexample.com%2Fpaper%3Fx%3D1&source=gmail"
            )
            .as_deref(),
            Some("https://example.com/paper?x=1")
        );
    }

    #[test]
    fn a_configured_mailbox_is_provenance_not_a_project_connection() {
        let document = MailDocument::new(
            "spark@example.com".to_owned(),
            "Interesting observation".to_owned(),
            "A self-contained observation with no project relationship.",
            None,
        )
        .unwrap();
        let candidate = document.into_candidates().remove(0);
        assert!(
            candidate
                .possible_connection
                .as_deref()
                .unwrap()
                .contains("no project connection is claimed")
        );
    }
}
