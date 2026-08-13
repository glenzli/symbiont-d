import DOMPurify from "dompurify";
import katex from "katex";
import "katex/dist/katex.min.css";
import { marked } from "marked";

import { protectMath, repairLeakedMathPlaceholders } from "./math-delimiters.mjs";

marked.setOptions({
  breaks: true,
  gfm: true,
});

export function renderRichText(target, source, options = {}) {
  const markdown = repairLeakedMathPlaceholders(source);
  const { protectedMarkdown, math } = protectMath(markdown);
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
    if (part.type === "topic" && part.topic?.topicId) {
      const topic = document.createElement("button");
      topic.type = "button";
      topic.className = "message-topic-reference";
      topic.dataset.messageTopicId = part.topic.topicId;
      topic.title = "查看主题";
      const label = document.createElement("small");
      label.textContent = "主题";
      const title = document.createElement("span");
      title.textContent = part.topic.title || "未命名主题";
      topic.append(label, title);
      target.append(topic);
    } else if (part.type === "markdown") {
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
    } else if (part.type === "quote" && part.quote?.sourceRevisionId) {
      const quote = document.createElement("button");
      quote.type = "button";
      quote.className = "message-quote";
      quote.dataset.sourceRevisionId = part.quote.sourceRevisionId;
      quote.title = "跳到引用原文";
      const meta = document.createElement("span");
      meta.className = "message-quote-meta";
      meta.textContent = [
        part.quote.sourceRole === "user" ? "你" : "symbiont-d",
        formatQuoteTime(part.quote.sourceAt),
        part.quote.wholeMessage ? "整条消息" : "所选片段",
      ]
        .filter(Boolean)
        .join(" · ");
      const excerpt = document.createElement("span");
      excerpt.className = "message-quote-text";
      excerpt.textContent = part.quote.text;
      quote.append(meta, excerpt);
      target.append(quote);
    } else if (part.type === "externalInput" && part.input) {
      const reference = document.createElement("details");
      reference.className = "message-external-input-reference";
      if (part.input.sourceRevisionId) {
        reference.dataset.sourceRevisionId = part.input.sourceRevisionId;
      }
      const summary = document.createElement("summary");
      const label = document.createElement("small");
      label.textContent = `来源 · ${part.input.actorName || "外部输入"}`;
      const title = document.createElement("span");
      title.textContent = part.input.title || "外部输入";
      summary.append(label, title);
      const body = document.createElement("div");
      body.className = "message-external-input-body";
      const excerpt = document.createElement("p");
      excerpt.textContent = part.input.excerpt || "";
      const meta = document.createElement("small");
      meta.textContent = [
        formatQuoteTime(part.input.observedAt),
        part.input.sourceCount ? `${part.input.sourceCount} 个来源` : "",
      ]
        .filter(Boolean)
        .join(" · ");
      body.append(excerpt, meta);
      reference.append(summary, body);
      target.append(reference);
    }
  }
}

function formatQuoteTime(value) {
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
