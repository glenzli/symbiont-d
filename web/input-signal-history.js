const DAY_MS = 24 * 60 * 60 * 1000;

// observedAt is delivery time, never the source document or event time.
export function shouldShowSignal(signal, repliedIds = [], now = Date.now()) {
  if (!signal?.id || signal.hidden || signal.dismissed || signal.duplicateOfSignalId) return false;
  if (signal.kind === "attacker_challenge") return false;
  if (repliedIds.includes(signal.id)) return true;
  const delivered = Date.parse(signal.observedAt || signal.observed_at || "");
  // Unknown clocks cannot prove expiry.
  return !Number.isFinite(delivered) || now - delivered <= DAY_MS;
}

// Called only during deliberate backward browsing, never by a timer. Preserve
// visible cards and compensate for removals above the reader's current anchor.
export function pruneExpiredSignals({ conversation, signals, repliedIds = [], onRemove = () => {}, regroup = () => {}, now = Date.now() }) {
  const bounds = conversation.getBoundingClientRect();
  const visible = (element) => {
    if (!element.getClientRects().length) return false;
    const rect = element.getBoundingClientRect();
    return rect.bottom > bounds.top && rect.top < bounds.bottom;
  };
  const anchor = [...conversation.querySelectorAll(".message")].find(visible);
  const anchorTop = anchor?.getBoundingClientRect().top;
  const byId = new Map(signals.map((signal) => [signal.id, signal]));
  let removed = 0;
  for (const article of conversation.querySelectorAll(".input-signal[data-signal-id]")) {
    const id = article.dataset.signalId;
    const signal = byId.get(id);
    if (!signal || shouldShowSignal(signal, repliedIds, now) || visible(article) || article.contains(document.activeElement) || document.querySelector("#signal-reply-tray [data-signal-id]")?.dataset.signalId === id) continue;
    onRemove(id);
    article.remove();
    removed += 1;
  }
  if (removed) {
    regroup();
    if (anchor?.isConnected) {
      const behavior = conversation.style.scrollBehavior;
      conversation.style.scrollBehavior = "auto";
      conversation.scrollTop += anchor.getBoundingClientRect().top - anchorTop;
      conversation.style.scrollBehavior = behavior;
    }
  }
  return removed;
}

// A source replied to from the archive may arrive after its historical peers.
// Insert by delivery/message time, including when older chat pages are loaded.
export function placeTimelineItem(conversation, article) {
  for (const group of conversation.querySelectorAll(":scope > .input-signal-group")) {
    group.replaceWith(...group.querySelector(":scope > .input-signal-group-items").children);
  }
  const timestamp = (element) => Date.parse(element.querySelector("time")?.dateTime || "");
  const at = timestamp(article);
  const next = [...conversation.querySelectorAll(":scope > .message")]
    .find((element) => element !== article && timestamp(element) > at);
  conversation.insertBefore(article, next || null);
}
