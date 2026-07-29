import DOMPurify from "dompurify";
import katex from "katex";
import "katex/dist/katex.min.css";
import { marked } from "marked";

marked.setOptions({
  breaks: true,
  gfm: true,
});

const blockMath = /\$\$([\s\S]+?)\$\$/g;
const inlineMath = /(?<!\\)\$([^$\n]+?)(?<!\\)\$/g;

export function renderRichText(target, source, options = {}) {
  const markdown = String(source || "");
  const math = [];
  const protectedMarkdown = markdown
    .replace(blockMath, (_, expression) => mathToken(math, expression, true))
    .replace(inlineMath, (_, expression) => mathToken(math, expression, false));
  const rendered = marked.parse(protectedMarkdown);
  target.innerHTML = DOMPurify.sanitize(rendered, {
    USE_PROFILES: { html: true },
    ADD_ATTR: ["target"],
  });

  for (const [index, item] of math.entries()) {
    const token = target.querySelector(`[data-symbiont-math="${index}"]`);
    if (!token) continue;
    if (options.streaming) {
      token.textContent = item.display ? `$$${item.expression}$$` : `$${item.expression}$`;
      continue;
    }
    try {
      katex.render(item.expression, token, {
        displayMode: item.display,
        output: "htmlAndMathml",
        strict: "ignore",
        throwOnError: false,
        trust: false,
      });
    } catch {
      token.textContent = item.display ? `$$${item.expression}$$` : `$${item.expression}$`;
    }
  }

  for (const link of target.querySelectorAll("a[href]")) {
    link.target = "_blank";
    link.rel = "noopener noreferrer";
  }
  for (const image of target.querySelectorAll("img")) {
    image.loading = "lazy";
    image.decoding = "async";
  }
}

export function renderMessageContent(target, entry, options = {}) {
  target.replaceChildren();
  const parts = entry?.parts?.length
    ? entry.parts
    : [{ type: "markdown", text: entry?.content || "" }];
  for (const part of parts) {
    if (part.type === "markdown") {
      const richText = document.createElement("div");
      richText.className = "rich-text";
      renderRichText(richText, part.text, options);
      target.append(richText);
    } else if (part.type === "image" && part.asset?.url) {
      const figure = document.createElement("figure");
      figure.className = "message-image";
      const image = document.createElement("img");
      image.src = part.asset.url;
      image.alt = part.asset.filename || "Attached image";
      image.loading = "lazy";
      image.decoding = "async";
      const caption = document.createElement("figcaption");
      caption.textContent = part.asset.filename || "Image";
      figure.append(image, caption);
      target.append(figure);
    }
  }
}

function mathToken(math, expression, display) {
  const index = math.push({ expression: expression.trim(), display }) - 1;
  return `<span data-symbiont-math="${index}"></span>`;
}
