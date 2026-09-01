const STORAGE_KEY = "symbiont-d:message-state:v1";
const BASE_TITLE = document.title;

export function initMessageSync({
  conversation,
  appendMessage,
  applyRuntime,
  shouldDeferMessages,
  runtimeQuery = () => ({}),
}) {
  const persisted = loadState();
  const knownRevisionIds = new Set();
  const assistantRevisionIds = new Set();
  const unreadRevisionIds = new Set(persisted?.unreadRevisionIds || []);
  const unreadElements = new Map();
  const seenRevisionIds = new Set();
  const pendingSeenRevisionIds = new Set();
  const unreadIndicator = document.querySelector("#unread-indicator");
  const unreadCount = document.querySelector("#unread-count");
  let observedAt = persisted?.observedAt || null;
  let cursor = null;
  let bootstrapped = false;
  let eventSource = null;
  let refreshing = false;
  let refreshQueued = false;
  let seenTimer = null;
  let reportedConnection = null;
  let readCheckFrame = null;

  const observer = new IntersectionObserver(
    (entries) => {
      if (!canRead()) return;
      for (const entry of entries) {
        if (entry.isIntersecting && entry.intersectionRatio >= 0.55) {
          markRead(entry.target.dataset.readId);
        }
      }
    },
    { root: conversation, threshold: [0.55] },
  );

  function track(element, message, options = {}) {
    const revisionId = message.revisionId;
    if (!revisionId) return;
    const isAssistant = message.role === "assistant";
    trackIncoming(element, revisionId, message.at, isAssistant, isAssistant, options);
    if (!options.history) cursor = revisionId;
  }

  function trackSignal(element, signal, options = {}) {
    if (!signal?.id) return;
    if (signal.kind === "attacker_challenge") {
      remove([`signal:${signal.id}`]);
      return;
    }
    trackIncoming(
      element,
      `signal:${signal.id}`,
      signal.observedAt || signal.observed_at,
      true,
      false,
      options,
    );
  }

  function trackIncoming(element, readId, at, unreadEligible, recordsSeen, options) {
    const alreadyKnown = knownRevisionIds.has(readId);
    if (options.previousElement) observer.unobserve(options.previousElement);
    knownRevisionIds.add(readId);
    element.dataset.readId = readId;
    if (recordsSeen) assistantRevisionIds.add(readId);
    observer.observe(element);
    const arrivedAfterLastVisit = persisted && at && (!persisted.observedAt || at > persisted.observedAt);
    if (
      unreadEligible &&
      !options.interactive &&
      (unreadRevisionIds.has(readId) || (!alreadyKnown && (options.incoming || arrivedAfterLastVisit)))
    ) {
      markUnread(element, readId);
    }
    observeTimestamp(at);
    persist();
    scheduleReadCheck();
  }

  function completeBootstrap(messages, signals = []) {
    if (!persisted) {
      unreadRevisionIds.clear();
      unreadElements.clear();
      document
        .querySelectorAll(".message.unread")
        .forEach((element) => clearUnreadElement(element));
    } else {
      for (const revisionId of [...unreadRevisionIds]) {
        if (!knownRevisionIds.has(revisionId)) unreadRevisionIds.delete(revisionId);
      }
    }
    for (const message of messages) observeTimestamp(message.at);
    for (const signal of signals) observeTimestamp(signal.observedAt || signal.observed_at);
    bootstrapped = true;
    persist();
    renderUnread();
    scheduleReadCheck();
  }

  async function refresh() {
    if (refreshing) {
      // SSE notifications can arrive while a bounded snapshot is in flight.
      // Preserve one follow-up pass so the event is never silently dropped.
      refreshQueued = true;
      return;
    }
    refreshing = true;
    try {
      const query = new URLSearchParams();
      if (cursor) query.set("afterRevisionId", cursor);
      for (const [name, value] of Object.entries(runtimeQuery())) {
        if (value !== undefined && value !== null) query.set(name, String(value));
      }
      const queryString = query.toString();
      const response = await fetch(`/api/runtime${queryString ? `?${queryString}` : ""}`, {
        cache: "no-store",
      });
      if (!response.ok) {
        reportConnection(false);
        return;
      }
      reportConnection(true);
      const payload = await response.json();
      applyRuntime(payload);
      if (shouldDeferMessages()) return;

      for (const message of payload.messages || []) {
        if (message.revisionId) cursor = message.revisionId;
        observeTimestamp(message.at);
        if (message.role !== "assistant") continue;
        if (
          message.revisionId &&
          knownRevisionIds.has(message.revisionId)
        ) {
          continue;
        }
        const follow = canRead() && isNearConversationEnd();
        appendMessage(message, { incoming: true, scroll: follow });
      }
      persist();
    } catch {
      reportConnection(false);
      // The event stream's next open event retries this cursor-based recovery.
    } finally {
      refreshing = false;
      if (refreshQueued) {
        refreshQueued = false;
        void refresh();
      }
    }
  }

  function start() {
    eventSource?.close();
    eventSource = new EventSource("/api/events");
    eventSource.addEventListener("open", () => {
      reportConnection(true);
      void refresh();
    });
    eventSource.addEventListener("runtime", () => {
      void refresh();
    });
    eventSource.addEventListener("error", () => {
      reportConnection(false);
      // EventSource reconnects with browser-managed backoff. An `open` event
      // always follows with one cursor-based refresh to close any gap.
    });
    refresh();
  }

  function remove(revisionIds) {
    for (const revisionId of revisionIds) {
      knownRevisionIds.delete(revisionId);
      assistantRevisionIds.delete(revisionId);
      unreadRevisionIds.delete(revisionId);
      const element = unreadElements.get(revisionId);
      if (element) {
        observer.unobserve(element);
        clearUnreadElement(element);
      }
      unreadElements.delete(revisionId);
    }
    const remaining = [
      ...conversation.querySelectorAll(".message[data-revision-id]"),
    ].filter(
      (element) => !revisionIds.includes(element.dataset.revisionId),
    );
    cursor = remaining.at(-1)?.dataset.revisionId || null;
    persist();
    renderUnread();
  }

  function markUnread(element, revisionId) {
    unreadRevisionIds.add(revisionId);
    unreadElements.set(revisionId, element);
    element.classList.add("unread");
    let marker = element.querySelector(".message-unread");
    if (!marker) {
      marker = document.createElement("span");
      marker.className = "message-unread";
      marker.textContent = "未读";
      element.querySelector(".message-meta")?.append(marker);
    }
    marker.hidden = false;
    renderUnread();
  }

  function markRead(revisionId) {
    if (!revisionId) return;
    queueSeen(revisionId);
    if (!unreadRevisionIds.delete(revisionId)) return;
    const element = unreadElements.get(revisionId);
    if (element) clearUnreadElement(element);
    unreadElements.delete(revisionId);
    persist();
    renderUnread();
  }

  function queueSeen(revisionId) {
    if (!assistantRevisionIds.has(revisionId)) return;
    if (seenRevisionIds.has(revisionId)) return;
    seenRevisionIds.add(revisionId);
    pendingSeenRevisionIds.add(revisionId);
    clearTimeout(seenTimer);
    seenTimer = setTimeout(flushSeen, 250);
  }

  async function flushSeen() {
    const revisionIds = [...pendingSeenRevisionIds];
    pendingSeenRevisionIds.clear();
    if (!revisionIds.length) return;
    try {
      await fetch("/api/interaction/seen", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          revisionIds,
          occurredAt: new Date().toISOString(),
        }),
        keepalive: true,
      });
    } catch {
      for (const revisionId of revisionIds) {
        seenRevisionIds.delete(revisionId);
      }
    }
  }

  function clearUnreadElement(element) {
    element.classList.remove("unread");
    const marker = element.querySelector(".message-unread");
    if (marker) marker.hidden = true;
  }

  function scanVisibleUnread() {
    if (!canRead()) return;
    const root = conversation.getBoundingClientRect();
    for (const [revisionId, element] of unreadElements) {
      if (!element.isConnected || element.getClientRects().length === 0) continue;
      const bounds = element.getBoundingClientRect();
      const visible =
        Math.min(bounds.bottom, root.bottom) - Math.max(bounds.top, root.top);
      if (bounds.height > 0 && root.height > 0 && visible >= Math.min(bounds.height, root.height) * 0.55) {
        markRead(revisionId);
      }
    }
  }

  function renderUnread() {
    const count = unreadRevisionIds.size;
    unreadIndicator.hidden = count === 0;
    unreadCount.textContent = String(count);
    unreadIndicator.setAttribute(
      "aria-label",
      count ? `${count} 条未读消息` : "没有未读消息",
    );
    document.title = count ? `(${count}) ${BASE_TITLE}` : BASE_TITLE;
    notifyNative({ type: "unread", count });
    renderUnreadDivider();
  }

  function renderUnreadDivider() {
    conversation.querySelector(".conversation-unread-divider")?.remove();
    const firstUnread = [...unreadElements.values()].find((element) => element.isConnected);
    if (!firstUnread) return;
    const divider = document.createElement("div");
    divider.className = "conversation-unread-divider";
    divider.setAttribute("role", "separator");
    divider.textContent = "上次读到这里 · 以下为新内容";
    firstUnread.before(divider);
  }

  function reportConnection(connected) {
    if (reportedConnection === connected) return;
    reportedConnection = connected;
    notifyNative({ type: "connection", connected });
  }

  function persist() {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({
          observedAt,
          unreadRevisionIds: [...unreadRevisionIds],
        }),
      );
    } catch {
      // Read state is allowed to fall back to the current page lifetime.
    }
  }

  function observeTimestamp(at) {
    if (at && (!observedAt || at > observedAt)) observedAt = at;
  }

  function canRead() {
    return document.visibilityState === "visible" && document.hasFocus();
  }

  function isNearConversationEnd() {
    return (
      conversation.scrollHeight -
        conversation.scrollTop -
        conversation.clientHeight <
      80
    );
  }

  function scheduleReadCheck() {
    if (readCheckFrame !== null) return;
    readCheckFrame = requestAnimationFrame(() => requestAnimationFrame(() => {
      readCheckFrame = null;
      scanVisibleUnread();
    }));
  }

  unreadIndicator.addEventListener("click", () => {
    const firstUnread = [...unreadElements.values()][0];
    firstUnread?.scrollIntoView({ behavior: "smooth", block: "center" });
    scheduleReadCheck();
  });
  window.addEventListener("focus", scheduleReadCheck);
  document.addEventListener("visibilitychange", scheduleReadCheck);
  conversation.addEventListener("scroll", scheduleReadCheck, { passive: true });
  conversation.addEventListener("load", scheduleReadCheck, true);
  conversation.addEventListener("click", scheduleReadCheck);
  window.addEventListener("resize", scheduleReadCheck);

  renderUnread();
  return {
    completeBootstrap,
    refresh,
    refreshUnreadPresentation: renderUnreadDivider,
    remove,
    start,
    track,
    trackSignal,
  };
}

function notifyNative(payload) {
  try {
    window.webkit?.messageHandlers?.symbiontNative?.postMessage(payload);
  } catch {
    // The browser UI has no native host, and should behave exactly as before.
  }
}

function isIncomingAssistant(message) {
  return message.role === "assistant";
}

function loadState() {
  try {
    const value = JSON.parse(localStorage.getItem(STORAGE_KEY));
    if (!value || typeof value !== "object") return null;
    return {
      observedAt: typeof value.observedAt === "string" ? value.observedAt : null,
      unreadRevisionIds: Array.isArray(value.unreadRevisionIds)
        ? value.unreadRevisionIds.filter((item) => typeof item === "string")
        : [],
    };
  } catch {
    return null;
  }
}
