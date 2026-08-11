const STORAGE_KEY = "symbiont-d.conversation-focus";

export function normalizeConversationFocus(value) {
  return value === true || value === "focused" || value === "true";
}

export function conversationFocusPresentation(focused, hiddenCount = 0) {
  return focused
    ? {
        label: "显示全部外部输入",
        tooltip: "显示外部输入",
        icon: "eye-off",
        visibleLabel: `聚焦中 · 隐藏 ${hiddenCount} 条`,
      }
    : {
        label: "只看对话与已采用的来源",
        tooltip: "聚焦对话",
        icon: "eye",
        visibleLabel: "聚焦",
      };
}

export function countConversationExternalInputs(conversation) {
  return conversation.querySelectorAll(".input-signal").length;
}

export function initConversationFocusUi({
  conversation,
  button,
  buttonLabel,
  banner,
  hiddenCount,
  showAllButton,
  renderIcons,
  storage = window.localStorage,
  notify = () => {},
}) {
  let focused = normalizeConversationFocus(storage.getItem(STORAGE_KEY));

  function render() {
    const externalInputCount = countConversationExternalInputs(conversation);
    conversation.classList.toggle("is-conversation-focused", focused);
    const presentation = conversationFocusPresentation(focused, externalInputCount);
    button.hidden = externalInputCount === 0;
    button.setAttribute("aria-pressed", String(focused));
    button.setAttribute("aria-label", presentation.label);
    button.title = presentation.label;
    button.dataset.tooltip = presentation.tooltip;
    buttonLabel.textContent = presentation.visibleLabel;
    hiddenCount.textContent = String(externalInputCount);
    banner.hidden = !focused || externalInputCount === 0;
    const oldIcon = button.querySelector("svg, i[data-lucide]");
    const icon = document.createElement("i");
    icon.dataset.lucide = presentation.icon;
    icon.setAttribute("aria-hidden", "true");
    oldIcon?.replaceWith(icon);
    renderIcons(button);
  }

  function setFocused(next, { announce = false } = {}) {
    focused = Boolean(next);
    storage.setItem(STORAGE_KEY, focused ? "focused" : "all");
    render();
    if (announce) {
      const count = countConversationExternalInputs(conversation);
      notify(focused ? `已隐藏 ${count} 条独立外部输入` : "已显示全部外部输入");
    }
  }

  button.addEventListener("click", () => setFocused(!focused, { announce: true }));
  showAllButton.addEventListener("click", () => setFocused(false, { announce: true }));
  const observer = new MutationObserver(render);
  observer.observe(conversation, { childList: true, subtree: true });
  render();

  return {
    isFocused: () => focused,
    setFocused,
    refresh: render,
    destroy: () => observer.disconnect(),
  };
}
