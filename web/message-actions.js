export function initMessageActions({ conversation, isBusy, perform }) {
  const entries = new WeakMap();
  const states = new WeakMap();
  const failures = new WeakMap();

  conversation.addEventListener("click", async (event) => {
    const button = event.target.closest("[data-message-action]");
    if (!button || isBusy()) return;
    const message = button.closest(".message");
    const entry = entries.get(message);
    if (!message || !entry || message.dataset.actionBusy === "true") return;

    message.dataset.actionBusy = "true";
    refresh();
    try {
      await perform(button.dataset.messageAction, message, entry);
    } catch (error) {
      setState(message, "failed", error.message);
    } finally {
      delete message.dataset.actionBusy;
      refresh();
    }
  });

  function track(message, entry, options = {}) {
    if (entry.role !== "user") return;
    entries.set(message, entry);
    setState(
      message,
      options.deliveryState || entry.deliveryState || "delivered",
      options.failureReason,
    );
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
    const userMessages = [
      ...conversation.querySelectorAll('.message[data-role="user"]'),
    ];
    const latest = userMessages.at(-1);
    for (const message of userMessages) render(message, message === latest);
  }

  function render(message, isLatest) {
    const foot = message.querySelector(".message-foot");
    const stateLabel = foot.querySelector(".message-state");
    const actions = foot.querySelector(".message-actions");
    const state = states.get(message) || "delivered";
    const actionBusy = message.dataset.actionBusy === "true";
    stateLabel.textContent =
      state === "pending" ? "发送中" : state === "failed" ? "未完成" : "";
    stateLabel.title = failures.get(message) || "";
    message.classList.toggle("message-failed", state === "failed");
    actions.replaceChildren();

    if (isLatest && !isBusy() && !actionBusy) {
      if (state === "failed") {
        actions.append(actionButton("retry", "重试"), actionButton("delete", "删除"));
      } else if (state === "delivered") {
        actions.append(actionButton("edit", "编辑"), actionButton("recall", "撤回"));
      }
    }
    foot.hidden = ![
      foot.querySelector(".message-runtime")?.textContent,
      !foot.querySelector(".trace-button")?.hidden,
      stateLabel.textContent,
      actions.childElementCount,
    ].some(Boolean);
  }

  return { refresh, track, update };
}

function actionButton(action, label) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "message-action";
  button.dataset.messageAction = action;
  button.textContent = label;
  return button;
}
