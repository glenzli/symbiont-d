// The received text is the evidence. Legacy model summaries are optional views;
// deterministic duplicate-section removal has a separate, explicit identity.
export function signalContent(signal) {
  const received = signal.receivedText || signal.received_text || "";
  const content = signal.content || received || signal.summary || signal.title || "";
  if (signal.presentation === "condensed" && received.trim()) {
    return {
      text: received,
      alternative: content.trim() !== received.trim() ? { label: "旧摘要", text: content } : null,
    };
  }
  return {
    text: content,
    alternative: signal.presentation === "excerpted" && received.trim() !== content.trim()
      ? { label: "完整原文", text: received } : null,
  };
}

export function appendSignalDetails(container, signal, renderContent) {
  const doc = container.ownerDocument;
  const bar = doc.createElement("div");
  bar.className = "input-signal-details";
  function detail(label, className, body) {
    const details = doc.createElement("details");
    details.className = `input-signal-detail ${className}`;
    details.dataset.signalPopover = "";
    const summary = doc.createElement("summary");
    summary.textContent = label;
    body.classList.add("signal-popover-panel");
    details.append(summary, body);
    bar.append(details);
  }
  const { alternative } = signalContent(signal);
  if (alternative?.text) {
    const body = doc.createElement("div");
    renderContent(body, { content: alternative.text, parts: [{ type: "markdown", text: alternative.text }] });
    detail(alternative.label, "input-signal-original", body);
  }
  const rawQualification = signal.qualificationNote || signal.qualification_note;
  const qualification = rawQualification && signal.presentation === "condensed"
    ? `旧摘要的审阅说明：${rawQualification}` : rawQualification;
  const notes = [signal.presentation === "excerpted" ? "正文仅省略此前已投递的重复段落；完整原文仍可查看。" : "", qualification].filter(Boolean);
  if (notes.length) {
    const body = doc.createElement("p");
    body.textContent = notes.join("\n");
    detail("说明", "input-signal-qualification", body);
  }
  const list = doc.createElement("ul");
  for (const source of signal.sources || []) {
    let url;
    try { url = new URL(source.url); } catch { continue; }
    if (!["https:", "http:"].includes(url.protocol)) continue;
    const item = doc.createElement("li");
    const link = doc.createElement("a");
    link.href = url.href;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    // Transport provenance belongs in the tooltip, not three identical long labels.
    link.textContent = source.detail && !source.detail.startsWith("Linked through ")
      ? source.detail : `${url.hostname}${url.pathname === "/" ? "" : url.pathname}`;
    link.title = source.detail || url.href;
    item.append(link);
    list.append(item);
  }
  if (list.childElementCount) detail(`${list.childElementCount} 个来源`, "input-signal-sources", list);
  if (bar.childElementCount) container.append(bar);
  return bar;
}
