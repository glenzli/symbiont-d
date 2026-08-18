const TOP_LOAD_THRESHOLD_PX = 24;

// Owns the bounded transcript window. New-message polling remains in
// message-sync; this component only pages durable history when the user asks
// to move backwards through time.
export function initMessageHistory({
  conversation,
  button,
  prependMessages,
  notify = () => {},
}) {
  let beforeAt = null;
  let hasMore = false;
  let loading = false;

  function configure({ oldestAt, hasMore: nextHasMore }) {
    beforeAt = exclusiveBefore(oldestAt);
    hasMore = Boolean(nextHasMore && beforeAt);
    render();
  }

  function render() {
    button.hidden = !hasMore;
    button.disabled = loading;
    button.textContent = loading ? "正在加载更早消息…" : "加载更早消息";
  }

  async function load() {
    if (loading || !hasMore || !beforeAt) return false;
    loading = true;
    render();
    const previousHeight = conversation.scrollHeight;
    const previousTop = conversation.scrollTop;
    try {
      const response = await fetch(
        `/api/messages?beforeAt=${encodeURIComponent(beforeAt)}`,
        { cache: "no-store" },
      );
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(payload.error || "无法读取更早消息");
      const messages = Array.isArray(payload.messages) ? payload.messages : [];
      const older = messages.filter((message) => message?.at);
      if (older.length) {
        prependMessages(older);
        beforeAt = exclusiveBefore(older[0].at);
        const scrollBehavior = conversation.style.scrollBehavior;
        conversation.style.scrollBehavior = "auto";
        conversation.scrollTop = previousTop + conversation.scrollHeight - previousHeight;
        conversation.style.scrollBehavior = scrollBehavior;
      }
      hasMore = Boolean(payload.hasMore && older.length && beforeAt);
      return older.length > 0;
    } catch (error) {
      notify(error.message || "无法读取更早消息");
      return false;
    } finally {
      loading = false;
      render();
    }
  }

  button.addEventListener("click", () => {
    void load();
  });
  conversation.addEventListener(
    "scroll",
    () => {
      if (conversation.scrollTop <= TOP_LOAD_THRESHOLD_PX) void load();
    },
    { passive: true },
  );

  return { configure, load };
}

function exclusiveBefore(at) {
  const timestamp = Date.parse(at || "");
  return Number.isFinite(timestamp)
    ? new Date(timestamp - 1).toISOString()
    : null;
}
