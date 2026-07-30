const STORAGE_KEY = "symbiont-d:message-state:v1";
const BASE_TITLE = document.title;

export function initMessageSync({
  conversation,
  appendMessage,
  applyRuntime,
  shouldDeferMessages,
}) {
  const persisted = loadState();
  const knownRevisionIds = new Set();
  const unreadRevisionIds = new Set(persisted?.unreadRevisionIds || []);
  const unreadElements = new Map();
  const unreadIndicator = document.querySelector("#unread-indicator");
  const unreadCount = document.querySelector("#unread-count");
  let observedAt = persisted?.observedAt || null;
  let cursor = null;
  let bootstrapped = false;
  let pollTimer = null;
  let refreshing = false;

  const observer = new IntersectionObserver(
    (entries) => {
      if (!canRead()) return;
      for (const entry of entries) {
        if (entry.isIntersecting && entry.intersectionRatio >= 0.55) {
          markRead(entry.target.dataset.revisionId);
        }
      }
    },
    { root: conversation, threshold: [0.55] },
  );

  function track(element, message, options = {}) {
    const revisionId = message.revisionId;
    if (!revisionId) return;
    knownRevisionIds.add(revisionId);
    cursor = revisionId;
    observer.observe(element);

    const arrivedAfterLastVisit =
      persisted && message.at && (!observedAt || message.at > observedAt);
    if (
      isIncomingAssistant(message) &&
      !options.interactive &&
      (unreadRevisionIds.has(revisionId) ||
        options.incoming ||
        arrivedAfterLastVisit)
    ) {
      markUnread(element, revisionId);
    }
    observeTimestamp(message.at);
    persist();
    scheduleReadCheck();
  }

  function completeBootstrap(messages) {
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
    bootstrapped = true;
    persist();
    renderUnread();
    scheduleReadCheck();
  }

  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      const query = cursor
        ? `?afterRevisionId=${encodeURIComponent(cursor)}`
        : "";
      const response = await fetch(`/api/runtime${query}`, {
        cache: "no-store",
      });
      if (!response.ok) return;
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
      // A later poll recovers after a daemon restart or brief disconnect.
    } finally {
      refreshing = false;
    }
  }

  function start() {
    clearInterval(pollTimer);
    refresh();
    pollTimer = setInterval(refresh, 2500);
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
    if (!revisionId || !unreadRevisionIds.delete(revisionId)) return;
    const element = unreadElements.get(revisionId);
    if (element) clearUnreadElement(element);
    unreadElements.delete(revisionId);
    persist();
    renderUnread();
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
      const bounds = element.getBoundingClientRect();
      const visible =
        Math.min(bounds.bottom, root.bottom) - Math.max(bounds.top, root.top);
      if (visible >= Math.min(bounds.height * 0.55, root.height)) {
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
    requestAnimationFrame(() => requestAnimationFrame(scanVisibleUnread));
  }

  unreadIndicator.addEventListener("click", () => {
    const firstUnread = [...unreadElements.values()][0];
    firstUnread?.scrollIntoView({ behavior: "smooth", block: "center" });
    scheduleReadCheck();
  });
  window.addEventListener("focus", scheduleReadCheck);
  document.addEventListener("visibilitychange", scheduleReadCheck);

  renderUnread();
  return { completeBootstrap, start, track, refresh };
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
