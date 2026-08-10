export function initEphemeralDiscussionUi({
  input,
  notify,
  renderSnapshot,
  clearMessages,
  appendPromoted,
  canActivate,
}) {
  const toggle = document.querySelector("#temporary-discussion-toggle");
  const banner = document.querySelector("#temporary-discussion-banner");
  const finish = document.querySelector("#temporary-discussion-finish");
  const dialog = document.querySelector("#temporary-discussion-dialog");
  const conclusion = document.querySelector("#temporary-discussion-conclusion");
  const continueButton = document.querySelector("#temporary-discussion-continue");
  const discardButton = document.querySelector("#temporary-discussion-discard");
  const saveConclusionButton = document.querySelector(
    "#temporary-discussion-save-conclusion",
  );
  const saveTranscriptButton = document.querySelector(
    "#temporary-discussion-save-transcript",
  );
  const status = document.querySelector("#temporary-discussion-decision-status");

  let active = false;
  let serverActive = false;
  let held = false;
  let busy = false;

  function render() {
    toggle.setAttribute("aria-pressed", String(active));
    toggle.classList.toggle("active", active);
    banner.hidden = !active;
    document.body.classList.toggle("temporary-discussion-active", active);
    if (active) {
      input.placeholder = "临时讨论：会读取已有记忆，但这段对话不会写入…";
    } else {
      input.placeholder = "说点什么…";
    }
  }

  function applySnapshot(snapshot, shouldRender = true) {
    serverActive = Boolean(snapshot?.active);
    held = Boolean(snapshot?.held);
    busy = Boolean(snapshot?.busy);
    active = active || serverActive;
    render();
    if (shouldRender) renderSnapshot(snapshot || { turns: [] });
  }

  async function request(path, options = {}) {
    const response = await fetch(path, options);
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.error || "临时讨论操作失败");
    return payload;
  }

  async function restore() {
    const snapshot = await request("/api/temporary-discussion");
    active = Boolean(snapshot.active);
    applySnapshot(snapshot);
    return snapshot;
  }

  async function send(message) {
    const reply = await request("/api/temporary-discussion/messages", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message }),
    });
    applySnapshot(reply.snapshot);
    return reply;
  }

  async function interrupt() {
    return request("/api/temporary-discussion/interrupt", { method: "POST" });
  }

  async function openDecision() {
    if (busy) {
      notify("请先停止当前回复");
      return;
    }
    if (!serverActive) {
      await request("/api/temporary-discussion", { method: "DELETE" });
      active = false;
      clearMessages();
      render();
      return;
    }
    const snapshot = await request("/api/temporary-discussion/hold", {
      method: "POST",
    });
    applySnapshot(snapshot, false);
    status.textContent = "";
    dialog.showModal();
  }

  async function resume() {
    if (serverActive && held) {
      const snapshot = await request("/api/temporary-discussion/resume", {
        method: "POST",
      });
      applySnapshot(snapshot, false);
    }
    dialog.close();
    input.focus();
  }

  async function discard() {
    const snapshot = await request("/api/temporary-discussion", {
      method: "DELETE",
    });
    active = false;
    applySnapshot(snapshot, false);
    clearMessages();
    dialog.close();
    notify("临时讨论已丢弃");
    input.focus();
  }

  async function promote(kind, markdown = "") {
    status.textContent = "正在保留…";
    const entry = await request("/api/temporary-discussion/promote", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(
        kind === "conclusion" ? { kind, markdown } : { kind },
      ),
    });
    active = false;
    serverActive = false;
    held = false;
    busy = false;
    clearMessages();
    render();
    dialog.close();
    appendPromoted(entry);
    notify(kind === "conclusion" ? "结论已进入记忆" : "讨论过程已进入记忆");
    input.focus();
  }

  toggle.addEventListener("click", () => {
    if (!active) {
      if (!canActivate()) {
        notify("请先停止当前回复");
        return;
      }
      active = true;
      render();
      notify("已进入临时讨论：读取记忆，但不写入");
      input.focus();
      return;
    }
    openDecision().catch((error) => notify(error.message));
  });
  finish.addEventListener("click", () => {
    openDecision().catch((error) => notify(error.message));
  });
  continueButton.addEventListener("click", () => {
    resume().catch((error) => {
      status.textContent = error.message;
    });
  });
  discardButton.addEventListener("click", () => {
    discard().catch((error) => {
      status.textContent = error.message;
    });
  });
  saveConclusionButton.addEventListener("click", () => {
    const markdown = conclusion.value.trim();
    if (!markdown) {
      status.textContent = "先写下希望保留的结论";
      conclusion.focus();
      return;
    }
    promote("conclusion", markdown).catch((error) => {
      status.textContent = error.message;
    });
  });
  saveTranscriptButton.addEventListener("click", () => {
    promote("full_transcript").catch((error) => {
      status.textContent = error.message;
    });
  });
  dialog.addEventListener("cancel", (event) => {
    event.preventDefault();
    resume().catch((error) => {
      status.textContent = error.message;
    });
  });

  render();
  return {
    isActive: () => active,
    isBusy: () => busy,
    restore,
    send,
    interrupt,
  };
}
