import MarkdownIt from "markdown-it";
import { tex } from "@mdit/plugin-tex";
import katex from "katex";
import { repairLeakedMathPlaceholders } from "./math-delimiters.mjs";

// Parse math in the Markdown grammar, before emphasis/escapes can consume TeX.
// Raw HTML is disabled. Sanitize the complete result at the DOM boundary too.
const markdown = new MarkdownIt({ html: false, breaks: true, linkify: true });
markdown.use(tex, {
  delimiters: "all",
  mathFence: true,
  allowInlineWithSpace: false,
  render(expression, displayMode) {
    const options = {
      displayMode, output: "htmlAndMathml", strict: "ignore", trust: false,
      maxExpand: 1000, maxSize: 20,
    };
    try {
      return katex.renderToString(expression, { ...options, throwOnError: true });
    } catch {
      // Some old MD documents over-escaped literal TeX punctuation (\\#).
      // Retry only failed formulas; never alter valid matrix/aligned row breaks.
      const repaired = expression.replace(/\\{2,}(?=[#%&_$])/g, "\\");
      if (repaired !== expression) {
        try { return katex.renderToString(repaired, { ...options, throwOnError: true }); }
        catch { /* Preserve the original expression when repair is inconclusive. */ }
      }
      return katex.renderToString(expression, { ...options, throwOnError: false });
    }
  },
});

export function renderMarkdown(source) {
  // Incomplete streaming expressions remain readable text until closed. There
  // is no separate streaming parser and no private marker to leak into storage.
  return markdown.render(repairLeakedMathPlaceholders(source));
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
