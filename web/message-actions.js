import { renderIcons } from "/icons.js";

const ACTIONS = {
  copy: { icon: "copy", label: "复制文本" },
  delete: { icon: "trash-2", label: "删除本条及后续对话", destructive: true },
  edit: { icon: "pencil", label: "从此处编辑并重开对话" },
  quote: { icon: "quote", label: "引用此消息" },
  recall: { icon: "undo-2", label: "撤回本条及后续对话", destructive: true },
  retry: { icon: "rotate-cw", label: "重新发送" },
};

export function initMessageActions({ conversation, isBusy, perform }) {
  const entries = new WeakMap();
  const states = new WeakMap();
  const failures = new WeakMap();

  conversation.addEventListener("click", async (event) => {
    const button = event.target.closest("[data-message-action]");
    const action = button?.dataset.messageAction;
    if (!button || (!["quote", "copy"].includes(action) && isBusy())) return;
    const message = button.closest(".message");
    const entry = entries.get(message);
    if (!message || !entry || message.dataset.actionBusy === "true") return;

    message.dataset.actionBusy = "true";
    refresh();
    try {
      await perform(action, message, entry);
    } catch (error) {
      setState(message, "failed", error.message);
    } finally {
      delete message.dataset.actionBusy;
      refresh();
    }
  });

  function track(message, entry, options = {}) {
    entries.set(message, entry);
    if (entry.role === "user") {
      setState(
        message,
        options.deliveryState || entry.deliveryState || "delivered",
        options.failureReason,
      );
    } else {
      refresh();
    }
  }

  function update(message, entry, options = {}) {
    if (entry) entries.set(message, entry);
    setState(
      message,
      options.deliveryState ||
        entry?.deliveryState ||
        states.get(message) ||
        "delivered",
      options.failureReason,
    );
  }

  function setState(message, state, failureReason) {
    states.set(message, state);
    message.dataset.deliveryState = state;
    if (failureReason) failures.set(message, failureReason);
    else failures.delete(message);
    refresh();
  }

  function refresh() {
    for (const message of conversation.querySelectorAll(".message")) {
      render(message);
    }
  }

  function render(message) {
    const foot = message.querySelector(".message-foot");
    const stateLabel = foot.querySelector(".message-state");
    const actions = foot.querySelector(".message-actions");
    const state = states.get(message) || "delivered";
    const isDelivered = ["delivered", "settled", "reacted"].includes(state);
    const entry = entries.get(message);
    const actionBusy = message.dataset.actionBusy === "true";
    stateLabel.textContent =
      state === "pending"
        ? "等待回复"
        : state === "failed"
          ? "回复中断"
          : state === "stopped"
            ? "已停止"
            : state === "settled"
              ? "已读 · 对话已收束"
              : state === "reacted"
                ? "已回应"
                : "";
    stateLabel.title = failures.get(message) || "";
    message.classList.toggle("message-failed", state === "failed");
    actions.replaceChildren();

    if (!actionBusy && entry?.revisionId && isDelivered) {
      actions.append(actionButton("quote"));
    }
    if (!actionBusy && String(entry?.content || "").trim()) {
      actions.append(actionButton("copy"));
    }
    if (
      message.dataset.role === "user" &&
      !isBusy() &&
      !actionBusy
    ) {
      if (state === "failed" || state === "stopped") {
        actions.append(actionButton("retry"), actionButton("delete"));
      } else if (isDelivered) {
        actions.append(actionButton("edit"), actionButton("recall"));
      }
    }
    renderIcons(actions);
    foot.hidden = ![
      foot.querySelector(".message-runtime")?.textContent,
      !foot.querySelector(".trace-button")?.hidden,
      stateLabel.textContent,
      actions.childElementCount,
    ].some(Boolean);
  }

  return {
    entryFor(message) {
      return entries.get(message);
    },
    refresh,
    track,
    update,
  };
}

function actionButton(action) {
  const definition = ACTIONS[action];
  const button = document.createElement("button");
  button.type = "button";
  button.className = definition.destructive
    ? "message-action message-action-danger"
    : "message-action";
  button.dataset.messageAction = action;
  button.dataset.tooltip = definition.label;
  button.title = definition.label;
  button.setAttribute("aria-label", definition.label);
  const icon = document.createElement("i");
  icon.dataset.lucide = definition.icon;
  icon.setAttribute("aria-hidden", "true");
  button.append(icon);
  return button;
}
