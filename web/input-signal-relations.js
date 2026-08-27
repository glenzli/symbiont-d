const SVG_NAMESPACE = "http://www.w3.org/2000/svg";
const DEFAULT_MAX_CONNECTOR_PX = 1200;

export function localRelationGeometry(
  sourceAvatar,
  dissentAvatar,
  viewport,
  maxConnectorPx = DEFAULT_MAX_CONNECTOR_PX,
) {
  if (!sourceAvatar || !dissentAvatar || !viewport) return null;
  const sourceHeight = sourceAvatar.height ?? sourceAvatar.bottom - sourceAvatar.top;
  const dissentHeight = dissentAvatar.height ?? dissentAvatar.bottom - dissentAvatar.top;
  const startX = sourceAvatar.left - viewport.left - 1;
  const startY = sourceAvatar.top + sourceHeight / 2 - viewport.top;
  const endX = dissentAvatar.left - viewport.left - 1;
  const endY = dissentAvatar.top + dissentHeight / 2 - viewport.top;
  const length = endY - startY;
  const visible =
    sourceAvatar.bottom > viewport.top &&
    sourceAvatar.top < viewport.bottom &&
    dissentAvatar.bottom > viewport.top &&
    dissentAvatar.top < viewport.bottom;
  if (!visible || length < 12 || length > maxConnectorPx) return null;
  const gutterX = Math.max(
    12,
    Math.min(sourceAvatar.left, dissentAvatar.left) - viewport.left - 4,
  );
  const horizontalSpan = Math.min(startX - gutterX, endX - gutterX);
  if (horizontalSpan < 2) return null;
  const radius = Math.min(6, horizontalSpan / 2, length / 4);
  return {
    startX,
    startY,
    endX,
    endY,
    gutterX,
    path: [
      `M ${startX} ${startY}`,
      `H ${gutterX + radius}`,
      `Q ${gutterX} ${startY} ${gutterX} ${startY + radius}`,
      `V ${endY - radius}`,
      `Q ${gutterX} ${endY} ${gutterX + radius} ${endY}`,
      `H ${endX}`,
    ].join(" "),
  };
}

function isVisibleInViewport(rect, viewport) {
  return Boolean(
    rect &&
      rect.bottom > viewport.top &&
      rect.top < viewport.bottom &&
      rect.right > viewport.left &&
      rect.left < viewport.right,
  );
}

export function relationAnchorGeometry(messageRect, groupRect, avatarRect, viewport) {
  if (!messageRect || !viewport) return null;
  if (isVisibleInViewport(avatarRect, viewport)) return avatarRect;

  // A grouped run has one shared avatar in its header. Once that header has
  // scrolled away, retain the same visual lane beside the still-visible card
  // instead of letting the connector lose its endpoint.
  const groupInset = groupRect ? Math.min(28, Math.max(16, groupRect.width * 0.06)) : 0;
  const left = groupRect
    ? groupRect.left + groupInset
    : Math.max(viewport.left + 24, messageRect.left - 24);
  const height = Math.min(40, Math.max(28, messageRect.height));
  const top = Math.max(messageRect.top, viewport.top);
  return {
    left,
    top,
    right: left + 1,
    bottom: top + height,
    width: 1,
    height,
  };
}

export function dissentPreview(message, maxLength = 86) {
  const text = `${message?.querySelector?.(".message-body")?.textContent || ""}`
    .replace(/\s+/g, " ")
    .trim();
  if (!text) return "查看这条异议";
  return text.length > maxLength ? `${text.slice(0, maxLength).trimEnd()}…` : text;
}

export function openRelationMarkerIds(markers) {
  return new Set(
    [...markers]
      .filter((marker) => marker.open && marker.dataset.sourceSignalId)
      .map((marker) => marker.dataset.sourceSignalId),
  );
}

export function initInputSignalRelations(conversation) {
  const frame = conversation?.closest(".conversation-frame");
  if (!conversation || !frame) {
    return {
      refresh() {},
      focusSources() {},
    };
  }

  const overlay = document.createElementNS(SVG_NAMESPACE, "svg");
  overlay.classList.add("input-signal-relation-lines");
  overlay.setAttribute("aria-hidden", "true");
  frame.append(overlay);
  let emphasizedIds = new Set();
  let openMarkerIds = new Set();
  let updateFrame = null;
  const layoutObserver = new ResizeObserver(scheduleLines);

  function revealGrouped(message) {
    const group = message.closest(".input-signal-group");
    if (!group?.classList.contains("is-collapsed")) return;
    group.classList.remove("is-collapsed");
    const toggle = group.querySelector(".input-signal-group-toggle");
    if (toggle) {
      toggle.setAttribute("aria-expanded", "true");
      toggle.textContent = "收起";
    }
  }

  function highlight(message) {
    message.classList.remove("quote-source-highlight");
    window.requestAnimationFrame(() => message.classList.add("quote-source-highlight"));
    window.setTimeout(() => message.classList.remove("quote-source-highlight"), 1600);
  }

  function relatedIds(dissent) {
    try {
      const ids = JSON.parse(dissent.dataset.relatedSignalIds || "[]");
      return Array.isArray(ids) ? ids : [];
    } catch {
      return [];
    }
  }

  function sourceMessage(id) {
    return conversation.querySelector(
      `.input-signal[data-signal-id="${CSS.escape(id)}"]`,
    );
  }

  function relationAvatar(message) {
    const groupAvatar = message
      ?.closest(".input-signal-group")
      ?.querySelector(".input-signal-group-avatar");
    return groupAvatar || message?.querySelector(".message-avatar");
  }

  function relationAnchor(message, viewport) {
    const messageRect = message?.getBoundingClientRect();
    if (!messageRect || !isVisibleInViewport(messageRect, viewport)) return null;
    const groupRect = message.closest(".input-signal-group")?.getBoundingClientRect();
    return relationAnchorGeometry(
      messageRect,
      groupRect,
      relationAvatar(message)?.getBoundingClientRect(),
      viewport,
    );
  }

  function scheduleLines() {
    if (updateFrame !== null) return;
    updateFrame = window.requestAnimationFrame(() => {
      updateFrame = null;
      renderLines();
    });
  }

  function renderLines() {
    overlay.replaceChildren();
    const viewport = frame.getBoundingClientRect();
    overlay.setAttribute("viewBox", `0 0 ${viewport.width} ${viewport.height}`);
    for (const dissent of conversation.querySelectorAll(
      '.input-signal.attacker-challenge[data-related-signal-ids]',
    )) {
      if (dissent.getClientRects().length === 0) continue;
      const dissentAnchor = relationAnchor(dissent, viewport);
      if (!dissentAnchor) continue;
      for (const id of relatedIds(dissent)) {
        const source = sourceMessage(id);
        if (!source || source.getClientRects().length === 0) continue;
        const sourceAnchor = relationAnchor(source, viewport);
        const geometry = localRelationGeometry(sourceAnchor, dissentAnchor, viewport);
        if (!geometry) continue;
        const path = document.createElementNS(SVG_NAMESPACE, "path");
        path.classList.add("input-signal-relation-line");
        if (emphasizedIds.has(id)) path.classList.add("is-emphasized");
        path.setAttribute("d", geometry.path);
        overlay.append(path);
      }
    }
  }

  function emphasize(ids) {
    emphasizedIds = new Set(ids);
    scheduleLines();
    window.setTimeout(() => {
      emphasizedIds = new Set();
      scheduleLines();
    }, 1600);
  }

  function focusSources(ids, dissent = null) {
    const sources = ids.map(sourceMessage).filter(Boolean);
    if (!sources.length) return;
    for (const source of sources) revealGrouped(source);
    sources[0].scrollIntoView({ behavior: "smooth", block: "center" });
    for (const source of sources) highlight(source);
    if (dissent) highlight(dissent);
    emphasize(ids);
    scheduleLines();
  }

  function refresh() {
    layoutObserver.disconnect();
    layoutObserver.observe(conversation);
    for (const message of conversation.querySelectorAll(".input-signal.has-dissent-response")) {
      message.classList.remove("has-dissent-response");
    }
    openMarkerIds = openRelationMarkerIds(
      conversation.querySelectorAll(".input-signal-dissent-marker"),
    );
    for (const marker of conversation.querySelectorAll(".input-signal-dissent-marker")) {
      marker.remove();
    }
    const dissentBySourceId = new Map();
    for (const dissent of conversation.querySelectorAll(
      '.input-signal.attacker-challenge[data-related-signal-ids]',
    )) {
      layoutObserver.observe(dissent);
      for (const id of relatedIds(dissent)) {
        const source = sourceMessage(id);
        if (!source || source === dissent) continue;
        source.classList.add("has-dissent-response");
        layoutObserver.observe(source);
        const related = dissentBySourceId.get(id) || [];
        related.push(dissent);
        dissentBySourceId.set(id, related);
      }
    }

    for (const [id, dissents] of dissentBySourceId) {
      const source = sourceMessage(id);
      const runtime = source?.querySelector(".message-runtime");
      if (!source || !runtime || runtime.querySelector(".input-signal-dissent-marker")) continue;
      const marker = document.createElement("details");
      marker.className = "input-signal-dissent-marker";
      marker.dataset.sourceSignalId = id;
      marker.open = openMarkerIds.has(id);
      const summary = document.createElement("summary");
      summary.textContent = `↗ ${dissents.length > 1 ? `${dissents.length} 条异议` : "有异议"}`;
      summary.title = "展开关联异议";
      const panel = document.createElement("div");
      panel.className = "input-signal-dissent-panel";
      for (const dissent of dissents) {
        const jump = document.createElement("button");
        jump.type = "button";
        jump.className = "input-signal-dissent-link";
        jump.textContent = dissentPreview(dissent);
        jump.title = "定位到这条异议";
        jump.addEventListener("click", () => {
          revealGrouped(dissent);
          dissent.scrollIntoView({ behavior: "smooth", block: "center" });
          highlight(source);
          highlight(dissent);
          emphasize([id]);
          scheduleLines();
        });
        panel.append(jump);
      }
      marker.append(summary, panel);
      marker.addEventListener("toggle", () => {
        if (marker.open) openMarkerIds.add(id);
        else openMarkerIds.delete(id);
      });
      runtime.append(" ", marker);
    }
    scheduleLines();
  }

  conversation.addEventListener("scroll", scheduleLines, { passive: true });
  conversation.addEventListener("click", () => window.requestAnimationFrame(scheduleLines));
  conversation.addEventListener("load", scheduleLines, true);
  window.addEventListener("resize", scheduleLines);
  document.addEventListener("visibilitychange", scheduleLines);

  return { refresh, focusSources, scheduleLines };
}
