// Dissent is an annotation of its source, not another timeline/unread item.
// Project existing records as well as new ones; never rewrite or delete evidence.
const ISSUE_LABELS = [
  ["来源冲突", /(?:来源|数据|数值|口径)[^。！？；\n]{0,12}(?:冲突|互相矛盾|不一致)/u],
  ["范围外推", /(?:范围|结论)[^。！？；\n]{0,12}(?:扩大|扩张|外推)|外推成|泛化成|类比性外推/u],
  ["过度表述", /过强|强于(?:证据|论文|实际)|明显强于|夸大|过度(?:表述|陈述|概括|推断)|证据强度过头|把推测写成了事实/u],
  ["证据不足", /证据不足|缺乏证据|证据[^。！？；\n]{0,12}(?:未能|并未|没有)(?:证明|建立|支持)|不足以证明/u],
];

// A display hint from the reviewer's own wording, never a verdict or a filter.
// Ignore quoted claims and explicit denials; ambiguous reviews stay generic.
export function annotationLabel(annotation) {
  const text = [annotation.content, annotation.reviewReason || annotation.review_reason]
    .filter(Boolean).join("\n")
    .replace(/“[^”]*”|「[^」]*」|"[^"\n]*"|`[^`]*`/gu, "")
    .replace(/(?:并非|不是|不属于|不存在)[^，,。！？；\n]*(?:，|,)?/gu, "");
  return ISSUE_LABELS.find(([, pattern]) => pattern.test(text))?.[0] || "有异议";
}

function annotationLabels(annotations) {
  const labels = [...new Set(annotations.map(annotationLabel))];
  // Keep the card compact; each expanded entry still carries its own label.
  return labels.slice(0, 2).join(" · ") + (labels.length > 2 ? " 等" : "");
}

export function annotationsBySource(signals) {
  const sources = new Set(signals.filter(s => s.kind !== "attacker_challenge" && !s.hidden).map(s => s.id));
  const result = new Map();
  for (const signal of signals) {
    if (signal.kind !== "attacker_challenge" || signal.hidden) continue;
    const content = signal.content || signal.receivedText || signal.received_text || "";
    if (!content.trim()) continue;
    for (const id of new Set(signal.relatedSignalIds || signal.related_signal_ids || [])) {
      if (!sources.has(id)) continue;
      const entries = result.get(id) || [];
      if (!entries.some(s => s.id === signal.id || s.content === content)) {
        entries.push({ ...signal, content });
        result.set(id, entries);
      }
    }
  }
  return result;
}

export function attachAnnotations(card, annotations, renderContent, {
  body = card.querySelector(".message-body"),
} = {}) {
  const previous = card.querySelector(".input-signal-dissent-marker");
  const open = previous?.open || false;
  previous?.remove();
  card.querySelector(".input-signal-review-badge")?.remove();
  card.classList.toggle("has-dissent-response", annotations.length > 0);
  if (!annotations.length || !body) return;
  const doc = card.ownerDocument;
  const label = annotationLabels(annotations);
  const marker = doc.createElement("details");
  marker.className = "input-signal-dissent-marker";
  marker.dataset.signalPopover = "";
  marker.open = open;
  const summary = doc.createElement("summary");
  summary.className = "input-signal-review-badge";
  const caption = doc.createElement("span");
  caption.textContent = annotations.length > 1 ? `${label} · ${annotations.length}` : label;
  summary.append(caption);
  summary.title = `${caption.textContent}：点击查看具体依据。原文保留，不代表整条消息已被判错。`;
  summary.setAttribute("aria-label", `展开${label}的具体依据`);
  const panel = doc.createElement("div");
  panel.className = "input-signal-dissent-panel signal-popover-panel";
  for (const annotation of annotations) {
    const item = doc.createElement("section");
    item.className = "input-signal-review";
    const header = doc.createElement("small");
    const at = annotation.observedAt || annotation.observed_at;
    header.textContent = [annotationLabel(annotation), annotation.actor?.name, at ? new Date(at).toLocaleString() : ""].filter(Boolean).join(" · ");
    const content = doc.createElement("div");
    content.className = "rich-text";
    renderContent(content, { content: annotation.content });
    item.append(header, content);
    for (const source of annotation.sources || []) {
      let url;
      try { url = new URL(source.url); } catch { continue; }
      if (!["https:", "http:"].includes(url.protocol)) continue;
      const link = doc.createElement("a");
      link.href = url.href;
      link.textContent = source.detail || url.hostname;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      item.append(link);
    }
    panel.append(item);
  }
  marker.append(summary, panel);
  // Anchor on the body's top border without entering the rich-text/opacity
  // boundary: the floating evidence panel must remain opaque and viewport-bound.
  body.before(marker);
}

export function initInputSignalRelations(conversation, { getSignals, renderContent }) {
  const signatures = new WeakMap();
  function refresh() {
    const annotations = annotationsBySource(getSignals());
    for (const card of conversation.querySelectorAll(".input-signal[data-signal-id]")) {
      const entries = annotations.get(card.dataset.signalId) || [];
      const signature = JSON.stringify(entries);
      if (signatures.get(card) === signature) continue;
      attachAnnotations(card, entries, renderContent);
      signatures.set(card, signature);
    }
  }
  return { refresh };
}
