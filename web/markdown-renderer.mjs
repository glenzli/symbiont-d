import MarkdownIt from "markdown-it";
import { tex } from "@mdit/plugin-tex";
import katex from "katex";
import { repairLeakedMathPlaceholders } from "./math-delimiters.mjs";

// Parse math in the Markdown grammar, before emphasis/escapes can consume TeX.
// Raw HTML is disabled. Sanitize the complete result at the DOM boundary too.
const markdown = new MarkdownIt({ html: false, breaks: true, linkify: true });

function repairLegacyDoubleEscapedNorms(expression) {
  const escapedNorm = String.raw`\\|`;
  const count = expression.split(escapedNorm).length - 1;

  // Some stored Markdown escaped TeX norm delimiters twice. KaTeX accepts the
  // leading `\\` as a line break, so this must be repaired before rendering
  // rather than in the parse-error fallback below. Keep structured multiline
  // environments untouched, where `\\` can be an intentional row separator.
  if (count >= 2 && count % 2 === 0 && !expression.includes(String.raw`\begin{`)) {
    return expression.replaceAll(escapedNorm, String.raw`\|`);
  }
  return expression;
}

function repairLegacyMathLinePrefixes(source) {
  // Generated Markdown occasionally used non-breaking-space entities only as
  // visual indentation before a display formula. That leaves `$$` mid-line,
  // so the Markdown grammar correctly treats it as literal text. Remove only
  // this legacy prefix; ordinary entities and indented code stay untouched.
  return source.replace(/^(?:&nbsp;)+(?=\$\$)/gm, "");
}

function repairSentenceAdjacentStrong(source) {
  // CommonMark leaves **句子。**下文 literal: punctuation before the closing
  // delimiter and a letter after it prevent the delimiter from closing.
  // Move only the punctuation outside an otherwise paired strong span. This
  // preserves visible text and avoids adding a space between Chinese words.
  return source.replace(
    /\*\*([^*\n]+?)([。！？；：，、.!?;:,])\*\*(?=[\p{L}\p{N}])/gu,
    "**$1**$2",
  );
}

markdown.core.ruler.before("inline", "repair_sentence_adjacent_strong", (state) => {
  for (const token of state.tokens) {
    // Code fences are separate block tokens. Be conservative with paragraphs
    // containing inline code, whose literal asterisks must stay untouched.
    if (token.type === "inline" && !token.content.includes("`")) {
      token.content = repairSentenceAdjacentStrong(token.content);
    }
  }
});

markdown.use(tex, {
  delimiters: "all",
  mathFence: true,
  allowInlineWithSpace: false,
  render(expression, displayMode) {
    const normalizedExpression = repairLegacyDoubleEscapedNorms(expression);
    const options = {
      displayMode, output: "htmlAndMathml", strict: "ignore", trust: false,
      maxExpand: 1000, maxSize: 20,
    };
    try {
      return katex.renderToString(normalizedExpression, { ...options, throwOnError: true });
    } catch {
      // Some old MD documents over-escaped literal TeX punctuation (\\#).
      // Retry only failed formulas; never alter valid matrix/aligned row breaks.
      const repaired = normalizedExpression.replace(/\\{2,}(?=[#%&_$])/g, "\\");
      if (repaired !== normalizedExpression) {
        try { return katex.renderToString(repaired, { ...options, throwOnError: true }); }
        catch { /* Preserve the original expression when repair is inconclusive. */ }
      }
      return katex.renderToString(normalizedExpression, { ...options, throwOnError: false });
    }
  },
});

export function renderMarkdown(source) {
  // Incomplete streaming expressions remain readable text until closed. There
  // is no separate streaming parser and no private marker to leak into storage.
  return markdown.render(repairLegacyMathLinePrefixes(repairLeakedMathPlaceholders(source)));
}

export function renderMarkdownInto(target, source, purifier) {
  target.innerHTML = purifier.sanitize(renderMarkdown(source), {
    USE_PROFILES: { html: true, mathMl: true, svg: true },
    ADD_TAGS: ["annotation"],
    ADD_ATTR: ["target", "encoding"],
  });
  for (const link of target.querySelectorAll("a[href]")) {
    link.target = "_blank";
    link.rel = "noopener noreferrer";
  }
  for (const image of target.querySelectorAll("img")) {
    image.loading = "lazy";
    image.decoding = "async";
  }
}
