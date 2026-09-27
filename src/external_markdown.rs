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
    let value = repair_escaped_dollar_delimited_math(&value);
    let value = repair_mixed_digest_escapes(&value);
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

/// Some external digests Markdown-escape the dollar delimiters and TeX
/// punctuation before delivery. In that form the Markdown parser sees literal
/// dollars and never invokes KaTeX. Recover only paired, math-shaped escaped
/// delimiters; a lone escaped price or a prose span stays literal.
fn repair_escaped_dollar_delimited_math(value: &str) -> String {
    let bytes = value.as_bytes();
    let has_escaped_tex_commands = bytes
        .windows(3)
        .any(|window| window[0] == b'\\' && window[1] == b'\\' && window[2].is_ascii_alphabetic());
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;

    while let Some((open, width, open_end)) = find_escaped_dollar_delimiter(bytes, cursor) {
        output.push_str(&value[cursor..open]);
        if !is_in_markdown_code(value, open)
            && let Some((close, close_end)) = find_matching_escaped_dollar(value, open_end, width)
        {
            let expression = &value[open_end..close];
            if escaped_span_looks_like_math(expression, width, has_escaped_tex_commands) {
                if width == 2 {
                    // A display delimiter after prose is not a Markdown block.
                    // Give it its own lines even when the source put both
                    // escaped delimiters in the middle of one list item.
                    output.push_str("\n\n$$\n");
                    let repaired = repair_escaped_math_punctuation(expression);
                    output.push_str(repaired.trim());
                    output.push_str("\n$$\n\n");
                } else {
                    output.push('$');
                    output.push_str(&repair_escaped_math_punctuation(expression));
                    output.push('$');
                }
                cursor = close_end;
                continue;
            }
        }
        output.push_str(&value[open..open_end]);
        cursor = open_end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn find_matching_escaped_dollar(
    value: &str,
    mut cursor: usize,
    width: usize,
) -> Option<(usize, usize)> {
    while let Some((start, closing_width, end)) =
        find_escaped_dollar_delimiter(value.as_bytes(), cursor)
    {
        if width == 1 && value.as_bytes()[cursor..start].contains(&b'\n') {
            return None;
        }
        if closing_width == width && !is_in_markdown_code(value, start) {
            return Some((start, end));
        }
        cursor = end;
    }
    None
}

fn is_in_markdown_code(value: &str, position: usize) -> bool {
    let prefix = &value[..position];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let mut fence = None::<(u8, usize)>;
    for line in prefix[..line_start].lines() {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            continue;
        }
        let marker = trimmed.as_bytes().first().copied();
        if !matches!(marker, Some(b'\x60' | b'~')) {
            continue;
        }
        let width = trimmed
            .as_bytes()
            .iter()
            .take_while(|byte| Some(**byte) == marker)
            .count();
        if width < 3 {
            continue;
        }
        match fence {
            None => fence = Some((marker.unwrap(), width)),
            Some((active, opening_width))
                if marker == Some(active)
                    && width >= opening_width
                    && trimmed[width..].trim().is_empty() =>
            {
                fence = None;
            }
            _ => {}
        }
    }
    if fence.is_some() {
        return true;
    }

    let line = prefix[line_start..].as_bytes();
    let mut cursor = 0;
    let mut inline_ticks = None;
    while cursor < line.len() {
        if line[cursor] != b'\x60' {
            cursor += 1;
            continue;
        }
        let width = line[cursor..]
            .iter()
            .take_while(|byte| **byte == b'\x60')
            .count();
        let escaped = cursor > 0
            && line[..cursor]
                .iter()
                .rev()
                .take_while(|byte| **byte == b'\\')
                .count()
                % 2
                == 1;
        if !escaped {
            match inline_ticks {
                None => inline_ticks = Some(width),
                Some(opening_width) if opening_width == width => inline_ticks = None,
                _ => {}
            }
        }
        cursor += width;
    }
    inline_ticks.is_some()
}

fn find_escaped_dollar_delimiter(bytes: &[u8], mut cursor: usize) -> Option<(usize, usize, usize)> {
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'\\'
            && bytes[cursor + 1] == b'$'
            && (cursor == 0 || bytes[cursor - 1] != b'\\')
        {
            if bytes.get(cursor + 2) == Some(&b'\\') && bytes.get(cursor + 3) == Some(&b'$') {
                return Some((cursor, 2, cursor + 4));
            }
            return Some((cursor, 1, cursor + 2));
        }
        cursor += 1;
    }
    None
}

fn escaped_span_looks_like_math(
    expression: &str,
    width: usize,
    has_escaped_tex_commands: bool,
) -> bool {
    if expression.is_empty() || width == 1 && expression.contains('\n') {
        return false;
    }
    let trimmed = expression.trim();
    if trimmed.is_empty() || width == 1 && trimmed != expression {
        return false;
    }
    let one_symbol =
        trimmed.chars().count() == 1 && trimmed.chars().all(|character| character.is_alphabetic());
    one_symbol
        || has_escaped_tex_commands && trimmed.chars().all(|character| character.is_ascii_digit())
        || trimmed.chars().any(|character| {
            matches!(
                character,
                '^' | '_'
                    | '\\'
                    | '='
                    | '+'
                    | '-'
                    | '*'
                    | '/'
                    | '{'
                    | '}'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '|'
            )
        })
}

fn repair_escaped_math_punctuation(expression: &str) -> String {
    let mut output = String::with_capacity(expression.len());
    let mut characters = expression.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\'
            && characters
                .peek()
                .is_some_and(|next| matches!(next, '_' | '=' | '-' | '<' | '>' | '[' | ']'))
        {
            output.push(characters.next().unwrap());
        } else {
            output.push(character);
        }
    }
    output
}

/// Repairs the narrower mixed state produced when a digest generator escapes
/// TeX and a few Markdown closers but leaves headings and list markers intact.
///
/// Do not collapse backslashes globally: `\\` is meaningful TeX. Restrict the
/// repair to paired dollar-delimited expressions that contain clear duplicate
/// escaping, plus the two malformed Markdown shapes observed at the digest
/// boundary.
fn repair_mixed_digest_escapes(value: &str) -> String {
    let malformed_link_closer = value.contains(r"\]\]](");
    let repaired_math = repair_dollar_delimited_math(value);
    let repair_horizontal_rule = malformed_link_closer || repaired_math != value;
    let value = repaired_math.replace(r"\]\]](", "]](");
    let value = value
        .lines()
        .map(|line| {
            if repair_horizontal_rule && line.trim() == r"\---" {
                "---"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    value
}

fn repair_dollar_delimited_math(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;

    while let Some(open) = find_unescaped_dollar(bytes, cursor) {
        output.push_str(&value[cursor..open]);
        let delimiter_width = if bytes.get(open + 1) == Some(&b'$') {
            2
        } else {
            1
        };
        let expression_start = open + delimiter_width;
        let Some(close) = find_matching_dollar(bytes, expression_start, delimiter_width) else {
            output.push_str(&value[open..]);
            return output;
        };
        output.push_str(&value[open..expression_start]);
        output.push_str(&repair_math_expression(&value[expression_start..close]));
        output.push_str(&value[close..close + delimiter_width]);
        cursor = close + delimiter_width;
    }

    output.push_str(&value[cursor..]);
    output
}

fn find_unescaped_dollar(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    while cursor < bytes.len() {
        if bytes[cursor] == b'$' && !is_escaped(bytes, cursor) {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn find_matching_dollar(bytes: &[u8], mut cursor: usize, delimiter_width: usize) -> Option<usize> {
    while cursor < bytes.len() {
        if bytes[cursor] == b'$'
            && !is_escaped(bytes, cursor)
            && (delimiter_width == 1 && bytes.get(cursor + 1) != Some(&b'$')
                || delimiter_width == 2 && bytes.get(cursor + 1) == Some(&b'$'))
        {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let mut preceding_backslashes = 0;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        preceding_backslashes += 1;
        cursor -= 1;
    }
    preceding_backslashes % 2 == 1
}

fn repair_math_expression(expression: &str) -> String {
    let characters = expression.chars().collect::<Vec<_>>();
    let has_duplicate_escape = duplicate_tex_escape_run(&characters).is_some();
    let has_spaced_escaped_equal = (0..characters.len()).any(|index| {
        characters[index] == '\\'
            && characters.get(index + 1) == Some(&'=')
            && (index == 0 || characters[index - 1].is_whitespace())
            && characters
                .get(index + 2)
                .map_or(true, |next| next.is_whitespace())
    });
    let has_escaped_script_bang = (0..characters.len()).any(|index| {
        matches!(characters[index], '_' | '^')
            && characters.get(index + 1) == Some(&'\\')
            && characters.get(index + 2) == Some(&'!')
    });
    let has_escaped_bracket = (0..characters.len()).any(|index| {
        characters[index] == '\\'
            && characters
                .get(index + 1)
                .is_some_and(|next| matches!(next, '[' | ']'))
    });
    let has_escaped_numeric_comparison = (0..characters.len())
        .any(|index| escaped_greater_is_numeric_comparison(&characters, index));
    if !has_duplicate_escape
        && !has_spaced_escaped_equal
        && !has_escaped_script_bang
        && !has_escaped_bracket
        && !has_escaped_numeric_comparison
    {
        return repair_invalid_brace_delimiters(expression);
    }

    let mut output = String::with_capacity(expression.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '\\' {
            let mut end = index + 1;
            while characters.get(end) == Some(&'\\') {
                end += 1;
            }
            if end - index >= 2
                && characters
                    .get(end)
                    .is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, '{' | '}'))
            {
                output.push('\\');
                index = end;
                continue;
            }
        }
        if characters[index] == '\\'
            && characters.get(index + 1) == Some(&'=')
            && (index == 0 || characters[index - 1].is_whitespace())
            && characters
                .get(index + 2)
                .map_or(true, |next| next.is_whitespace())
        {
            output.push('=');
            index += 2;
            continue;
        }
        if matches!(characters[index], '_' | '^')
            && characters.get(index + 1) == Some(&'\\')
            && characters.get(index + 2) == Some(&'!')
        {
            output.push(characters[index]);
            output.push('!');
            index += 3;
            continue;
        }
        if characters[index] == '\\'
            && let Some(bracket @ ('[' | ']')) = characters.get(index + 1).copied()
        {
            output.push(bracket);
            index += 2;
            continue;
        }
        if escaped_greater_is_numeric_comparison(&characters, index) {
            output.push('>');
            index += 2;
            continue;
        }
        output.push(characters[index]);
        index += 1;
    }
    repair_invalid_brace_delimiters(&output)
}

fn repair_invalid_brace_delimiters(expression: &str) -> String {
    // A bare brace starts or ends a TeX group; \left and \right require
    // escaped braces when the intended delimiter is visible.
    expression
        .replace(r"\left{", r"\left\{")
        .replace(r"\right}", r"\right\}")
}

fn escaped_greater_is_numeric_comparison(characters: &[char], index: usize) -> bool {
    if characters.get(index) != Some(&'\\') || characters.get(index + 1) != Some(&'>') {
        return false;
    }
    let Some(previous) = index.checked_sub(1).and_then(|index| characters.get(index)) else {
        return false;
    };
    let Some(next) = characters.get(index + 2) else {
        return false;
    };
    let previous_is_operand = previous.is_ascii_alphanumeric() || matches!(previous, ')' | ']');
    let next_is_operand = next.is_ascii_alphanumeric() || matches!(next, '(' | '[');
    previous_is_operand && next_is_operand && (previous.is_ascii_digit() || next.is_ascii_digit())
}

fn duplicate_tex_escape_run(characters: &[char]) -> Option<usize> {
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '\\' {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while characters.get(end) == Some(&'\\') {
            end += 1;
        }
        if end - index >= 2
            && characters
                .get(end)
                .is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, '{' | '}'))
        {
            return Some(index);
        }
        index = end;
    }
    None
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
    fn restores_escaped_math_from_a_drive_digest_without_changing_the_claims() {
        let value = r"1. **第二类速度奇异**：速率 \$(T-t)^{-1/2}\$；
2. **压力场非 \$L^2\$ 性**：压力场 \$p\$ 必须脱离 \$L^2\$；
3. **击穿 Onsager 空间**：脱离 \$L^3\_t B^{1/3}\_{3,c\_0}\$。
4. 能量式：\$\$\\frac{d}{dt} \\frac{1}{2}\\int\_{\mathbb{R}^3}|u|^2 dx \= -\\nu\\int|\nabla u|^2 dx\$\$";
        let expected = r"1. **第二类速度奇异**：速率 $(T-t)^{-1/2}$；
2. **压力场非 $L^2$ 性**：压力场 $p$ 必须脱离 $L^2$；
3. **击穿 Onsager 空间**：脱离 $L^3_t B^{1/3}_{3,c_0}$。
4. 能量式：

$$
\frac{d}{dt} \frac{1}{2}\int_{\mathbb{R}^3}|u|^2 dx = -\nu\int|\nabla u|^2 dx
$$";
        assert_eq!(normalize_external_markdown(value), expected);
        assert_eq!(normalize_external_markdown(expected), expected);
    }

    #[test]
    fn leaves_escaped_prices_and_prose_dollars_literal() {
        let value = r"价格从 \$5 到 \$6；字面写法 \$5\$，还有 \$a and b\$。";
        assert_eq!(normalize_external_markdown(value), value);
    }

    #[test]
    fn repairs_numeric_math_and_brace_delimiters_in_a_systematically_escaped_document() {
        let value = r"原点 \$0\$；
公式：\$\$\\left{ x \= 0 \\right}\$\$";
        let expected = r"原点 $0$；
公式：

$$
\left\{ x = 0 \right\}
$$";
        assert_eq!(normalize_external_markdown(value), expected);
        assert_eq!(normalize_external_markdown(expected), expected);
    }

    #[test]
    fn does_not_turn_escaped_dollars_inside_code_into_math() {
        let value =
            "字面 \x60\\$p\\$\x60，公式 \\$p\\$。\n\n\x60\x60\x60text\n\\$L^2\\$\n\x60\x60\x60";
        let expected =
            "字面 \x60\\$p\\$\x60，公式 $p$。\n\n\x60\x60\x60text\n\\$L^2\\$\n\x60\x60\x60";
        assert_eq!(normalize_external_markdown(value), expected);
    }

    #[test]
    fn repairs_mixed_digest_math_links_and_horizontal_rules() {
        let value = r#"* **论文/研究来源**：[arXiv:2608.11665 [math.CO\]\]](https://arxiv.org/abs/2608.11665)
* 当 $S=\\emptyset$ 时为普通 Nim，当 $S=\\{0\\}$ 时为 Misère Nim。
* 周期禁态集合为 $S \= d\\mathbb{N}_0$，且 $d \= 2, 3, 4$。
* 必胜态为 $\\mathcal{N}$，必败态为 $\\mathcal{P}$。

\---"#;
        assert_eq!(
            normalize_external_markdown(value),
            r#"* **论文/研究来源**：[arXiv:2608.11665 [math.CO]](https://arxiv.org/abs/2608.11665)
* 当 $S=\emptyset$ 时为普通 Nim，当 $S=\{0\}$ 时为 Misère Nim。
* 周期禁态集合为 $S = d\mathbb{N}_0$，且 $d = 2, 3, 4$。
* 必胜态为 $\mathcal{N}$，必败态为 $\mathcal{P}$。

---"#
        );
    }

    #[test]
    fn preserves_valid_tex_line_breaks_and_isolated_equal_accents() {
        let value = r"保留公式 $x \\ y$、$\\ \mathbb{N}$ 与 $a \=b$。";
        assert_eq!(normalize_external_markdown(value), value);
    }

    #[test]
    fn collapses_every_duplicate_escape_layer_before_tex_commands() {
        let value = r"变形函数 $\\\\mathfrak{S}C_\\\\alpha(v)$ 与 $\\\\kappa$。";
        let odd_layer = r"变形函数 $\\\mathfrak{S}C_\\\alpha(v)$ 与 $\\\kappa$。";
        let expected = r"变形函数 $\mathfrak{S}C_\alpha(v)$ 与 $\kappa$。";
        assert_eq!(normalize_external_markdown(value), expected);
        assert_eq!(normalize_external_markdown(odd_layer), expected);
        assert_eq!(normalize_external_markdown(expected), expected);
    }

    #[test]
    fn repairs_markdown_punctuation_escapes_that_break_or_change_tex() {
        let value = r"六大运算 $f^*, f_*, f_\!, f^\!, \otimes^\mathbb{L}, \mathcal{R}\mathcal{H}om$；特征 $p\>0$；局域域 $k\[\[t\]\]$。";
        let expected = r"六大运算 $f^*, f_*, f_!, f^!, \otimes^\mathbb{L}, \mathcal{R}\mathcal{H}om$；特征 $p>0$；局域域 $k[[t]]$。";

        assert_eq!(normalize_external_markdown(value), expected);
        assert_eq!(normalize_external_markdown(expected), expected);
    }

    #[test]
    fn preserves_valid_tex_spacing_and_literal_punctuation() {
        let value =
            r"保留间距 $\int\! f(x)\,dx$、字面下划线 $\mathrm{file\_name}$ 与旧式间距 $a\>b$。";
        assert_eq!(normalize_external_markdown(value), value);
    }

    #[test]
    fn preserves_an_isolated_escaped_horizontal_rule() {
        let value = "字面分隔符：\n\n\\---";
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
