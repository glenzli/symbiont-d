//! Normalizes attributed external text for readable Markdown presentation.
//!
//! External feeds often contain tracking redirects and naked URLs. Keep their
//! complete text, but present links as compact labels and point them at the
//! actual source whenever a redirect exposes one.

use std::collections::HashSet;

use reqwest::Url;

const MAX_SOURCE_URLS: usize = 8;

pub(crate) fn normalize_external_markdown(value: &str) -> String {
    let value = unescape_systematically_escaped_markdown(value);
    // Heal the one malformed wrapper produced by the earliest local migration
    // before normalized links became aware of angle-bracket autolinks.
    let value = value.replace("<[查看来源](<", "[查看来源](<");
    let mut output = Vec::<String>::new();
    for raw_line in value.lines() {
        let line = raw_line.trim_end();
        if let Some(url) = standalone_source_url(line) {
            if let Some(previous) = output.last_mut()
                && let Some(label) = call_to_action_label(previous)
            {
                *previous = markdown_link(&label, &url);
                continue;
            }
            output.push(markdown_link("打开来源", &url));
            continue;
        }
        output.push(normalize_inline_urls(line));
    }
    output.join("\n")
}

fn unescape_systematically_escaped_markdown(value: &str) -> String {
    let lines = value.lines().collect::<Vec<_>>();
    let escaped_heading = lines
        .iter()
        .any(|line| line.trim_start().starts_with("\\#\\#"));
    let escaped_list_items = lines
        .iter()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("\\* ") || line.starts_with("\\- ") || line.starts_with("\\+ ")
        })
        .count();
    let escaped_inline = value.contains("\\*\\*") || value.contains("\\](");
    if !(escaped_heading && (escaped_list_items > 0 || escaped_inline)
        || escaped_list_items >= 2 && escaped_inline)
    {
        return value.to_owned();
    }

    lines
        .into_iter()
        .map(|line| {
            let mut line = line
                .replace("\\#", "#")
                .replace("\\*", "*")
                .replace("\\_", "_")
                .replace("\\`", "`")
                .replace("\\~", "~")
                .replace("\\> ", "> ")
                .replace("\\- ", "- ")
                .replace("\\+ ", "+ ")
                .replace("\\. ", ". ");
            if line.contains("\\](") {
                line = line
                    .replace("\\[", "[")
                    .replace("\\](", "](")
                    .replace("\\!", "!");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn canonical_source_url(value: &str) -> Option<String> {
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
            if matches!(key.as_ref(), "q" | "url")
                && let Ok(target) = Url::parse(&value)
                && matches!(target.scheme(), "http" | "https")
            {
                return Some(target.to_string());
            }
        }
    }
    Some(parsed.to_string())
}

/// Extracts a bounded set of HTTP source URLs from plain text or Markdown.
///
/// Conversation messages are not an archival source registry, but URLs they
/// explicitly discuss are useful negative evidence for the next sensing pass.
pub(crate) fn source_urls(value: &str) -> Vec<String> {
    let mut remaining = value;
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    while urls.len() < MAX_SOURCE_URLS {
        let Some(start) = find_url_start(remaining) else {
            break;
        };
        let tail = &remaining[start..];
        let token_end = tail
            .char_indices()
            .find_map(|(index, character)| character.is_whitespace().then_some(index))
            .unwrap_or(tail.len());
        let token = tail[..token_end].trim_end_matches(|character: char| {
            matches!(
                character,
                ')' | ']' | '>' | '"' | '\'' | '.' | ',' | ';' | '，' | '。' | '；' | '、'
            )
        });
        if let Some(url) = canonical_source_url(token)
            && seen.insert(url.clone())
        {
            urls.push(url);
        }
        remaining = &tail[token_end..];
    }
    urls
}

fn standalone_source_url(line: &str) -> Option<String> {
    let trimmed = line
        .trim()
        .trim_matches(|character: char| matches!(character, '<' | '>' | '"' | '\'' | '(' | ')'));
    (!trimmed.chars().any(char::is_whitespace))
        .then(|| canonical_source_url(trimmed))
        .flatten()
}

fn call_to_action_label(line: &str) -> Option<String> {
    let mut label = line.trim();
    for suffix in ["→", "➡", "➜", "->", "：", ":"] {
        label = label.strip_suffix(suffix).unwrap_or(label).trim_end();
    }
    let has_link_intent = [
        "查看", "打开", "阅读", "详情", "来源", "链接", "论文", "报告", "项目", "官网",
    ]
    .iter()
    .any(|term| label.contains(term));
    (has_link_intent && !label.contains("http") && label.chars().count() <= 160)
        .then(|| label.to_owned())
}

fn normalize_inline_urls(line: &str) -> String {
    // Existing Markdown links are already deliberately labelled. Avoid
    // rewriting their destination and accidentally nesting another link.
    if line.contains("](") || line.contains("](<") {
        return line.to_owned();
    }

    let mut remaining = line;
    let mut output = String::new();
    while let Some(start) = find_url_start(remaining) {
        let tail = &remaining[start..];
        let token_end = tail
            .char_indices()
            .find_map(|(index, character)| character.is_whitespace().then_some(index))
            .unwrap_or(tail.len());
        let token = &tail[..token_end];
        let angle_wrapped = remaining[..start].ends_with('<') && token.ends_with('>');
        let prefix = if angle_wrapped {
            &remaining[..start - 1]
        } else {
            &remaining[..start]
        };
        output.push_str(prefix);
        let cleaned = token.trim_end_matches(|character: char| {
            matches!(
                character,
                '.' | ',' | ';' | '，' | '。' | '；' | '、' | ']' | '】' | '》' | '>'
            )
        });
        let punctuation = if angle_wrapped {
            ""
        } else {
            &token[cleaned.len()..]
        };
        if let Some(url) = canonical_source_url(cleaned) {
            output.push_str(&markdown_link("查看来源", &url));
            output.push_str(punctuation);
        } else {
            output.push_str(token);
        }
        remaining = &tail[token_end..];
    }
    output.push_str(remaining);
    output
}

fn find_url_start(value: &str) -> Option<usize> {
    [value.find("https://"), value.find("http://")]
        .into_iter()
        .flatten()
        .min()
}

fn markdown_link(label: &str, url: &str) -> String {
    let label = label.replace('[', "\\[").replace(']', "\\]");
    format!("[{label}](<{url}>)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_a_call_to_action_and_google_redirect_into_one_markdown_link() {
        let value = "查看 arXiv 论文（arXiv:2608.00086） →\nhttps://www.google.com/url?q=https%3A%2F%2Farxiv.org%2Fabs%2F2608.00086&source=gmail";
        assert_eq!(
            normalize_external_markdown(value),
            "[查看 arXiv 论文（arXiv:2608.00086）](<https://arxiv.org/abs/2608.00086>)"
        );
    }

    #[test]
    fn labels_an_unaccompanied_naked_url() {
        assert_eq!(
            normalize_external_markdown("补充材料 https://example.com/report"),
            "补充材料 [查看来源](<https://example.com/report>)"
        );
    }

    #[test]
    fn preserves_existing_markdown_links() {
        let value = "[查看论文](https://example.com/paper)";
        assert_eq!(normalize_external_markdown(value), value);
    }

    #[test]
    fn removes_angle_brackets_around_an_inline_tracking_url() {
        let value = "查看报告 → <https://www.google.com/url?q=https%3A%2F%2Fexample.com%2Fpaper&source=gmail> 下一项";
        assert_eq!(
            normalize_external_markdown(value),
            "查看报告 → [查看来源](<https://example.com/paper>) 下一项"
        );
    }

    #[test]
    fn repairs_the_early_migration_wrapper() {
        let value = "查看报告 → <[查看来源](<https://example.com/paper>)";
        assert_eq!(
            normalize_external_markdown(value),
            "查看报告 → [查看来源](<https://example.com/paper>)"
        );
    }

    #[test]
    fn restores_a_systematically_escaped_markdown_digest() {
        let value = "\\#\\#\\# 【天体物理】潮汐撕裂事件\n\\* \\*\\*论文/研究来源\\*\\*：\\[马里兰大学 / ScienceDaily\\](https://example.com/paper)\n\\* \\*\\*核心突破\\*\\*：发现一颗流浪黑洞。\n\n\\#\\#";
        assert_eq!(
            normalize_external_markdown(value),
            "### 【天体物理】潮汐撕裂事件\n* **论文/研究来源**：[马里兰大学 / ScienceDaily](https://example.com/paper)\n* **核心突破**：发现一颗流浪黑洞。\n\n##"
        );
    }

    #[test]
    fn preserves_isolated_markdown_escapes_and_math_delimiters() {
        let value = "使用 \\* 表示字面星号，并保留公式：\\[x + y\\]。";
        assert_eq!(normalize_external_markdown(value), value);
    }

    #[test]
    fn extracts_sources_from_markdown_and_naked_urls_without_duplicates() {
        let value = "[The Collaboration Tax](https://arxiv.org/abs/2608.22152) and https://arxiv.org/abs/2608.22152。";
        assert_eq!(source_urls(value), vec!["https://arxiv.org/abs/2608.22152"]);
    }

    #[test]
    fn extracted_sources_unwrap_google_redirects() {
        let value =
            "https://www.google.com/url?q=https%3A%2F%2Farxiv.org%2Fabs%2F2608.22152&source=gmail";
        assert_eq!(source_urls(value), vec!["https://arxiv.org/abs/2608.22152"]);
    }
}
