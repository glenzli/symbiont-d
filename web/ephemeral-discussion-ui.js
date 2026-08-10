export function initEphemeralDiscussionUi({
  notify,
  renderSnapshot,
  renderPending,
  clearMessages,
  appendPromoted,
  canActivate,
  onBusyChange,
}) {
  const shell = document.querySelector(".shell");
  const toggle = document.querySelector("#temporary-discussion-toggle");
  const layer = document.querySelector("#temporary-discussion-layer");
  const backdrop = document.querySelector("#temporary-discussion-backdrop");
  const finish = document.querySelector("#temporary-discussion-finish");
  const form = document.querySelector("#temporary-discussion-composer");
  const input = document.querySelector("#temporary-discussion-message");
  const sendButton = document.querySelector("#temporary-discussion-send");
  const stopButton = document.querySelector("#temporary-discussion-stop");
  const runtime = document.querySelector("#temporary-discussion-runtime");
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
  let stopping = false;

  function resizeInput() {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 180)}px`;
  }

  function focusInput() {
    requestAnimationFrame(() => {
      if (!active || held || dialog.open) return;
      input.focus();
      resizeInput();
    });
  }

  function render() {
    toggle.setAttribute("aria-pressed", String(active));
    toggle.classList.toggle("active", active);
    document.body.classList.toggle("temporary-discussion-active", active);
    layer.hidden = !active;
    shell.inert = active;
    shell.setAttribute("aria-hidden", String(active));
    input.disabled = busy || held;
    sendButton.disabled = busy || held || !input.value.trim();
    sendButton.hidden = busy;
    stopButton.hidden = !busy;
    stopButton.disabled = stopping;
    runtime.textContent = stopping
      ? "正在停止回复"
      : busy
        ? "正在回应 · 内容仍是临时的"
        : held
          ? "独立讨论已暂停"
          : "仅保留在当前进程";
  }

  function setBusy(next) {
    busy = next;
    if (!next) stopping = false;
    onBusyChange?.(next);
    render();
  }

  function applySnapshot(snapshot, shouldRender = true) {
    serverActive = Boolean(snapshot?.active);
    held = Boolean(snapshot?.held);
    active = active || serverActive;
    setBusy(Boolean(snapshot?.busy));
    if (shouldRender) renderSnapshot(snapshot || { turns: [] });
  }

  async function request(path, options = {}) {
    const response = await fetch(path, options);
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.error || "独立讨论操作失败");
    return payload;
  }

  async function restore() {
    const snapshot = await request("/api/temporary-discussion");
    active = Boolean(snapshot.active);
    applySnapshot(snapshot);
    if (active) focusInput();
    return snapshot;
  }

  async function restoreAuthoritativeSnapshot() {
    try {
      const snapshot = await request("/api/temporary-discussion");
      active = Boolean(snapshot.active);
      applySnapshot(snapshot);
      return true;
    } catch {
      return false;
    }
  }

  async function submitMessage(message) {
    renderPending(message);
    setBusy(true);
    try {
      const reply = await request("/api/temporary-discussion/messages", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ message }),
      });
      applySnapshot(reply.snapshot);
      if (reply.interrupted) notify("已停止回复");
    } catch (error) {
      const restored = await restoreAuthoritativeSnapshot();
      if (!restored) clearMessages();
      notify(error.message);
    } finally {
      setBusy(false);
      focusInput();
    }
  }

  async function interrupt() {
    if (!busy || stopping) return;
    stopping = true;
    render();
    try {
      const payload = await request("/api/temporary-discussion/interrupt", {
        method: "POST",
      });
      if (!payload.accepted) notify("回复已结束");
    } catch (error) {
      stopping = false;
      render();
      notify(error.message);
    }
  }

  async function exitLocalDiscussion() {
    await request("/api/temporary-discussion", { method: "DELETE" });
    active = false;
    serverActive = false;
    held = false;
    setBusy(false);
    clearMessages();
    conclusion.value = "";
  }

  async function openDecision() {
    if (busy) {
      notify("请先停止当前回复");
      return;
    }
    if (!serverActive) {
      await exitLocalDiscussion();
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
    focusInput();
  }

  async function discard() {
    await exitLocalDiscussion();
    dialog.close();
    notify("独立讨论已丢弃");
  }

  async function promote(kind, markdown = "") {
    status.textContent = "正在保留…";
    const entry = await request("/api/temporary-discussion/promote", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(kind === "conclusion" ? { kind, markdown } : { kind }),
    });
    active = false;
    serverActive = false;
    held = false;
    setBusy(false);
    clearMessages();
    conclusion.value = "";
    dialog.close();
    appendPromoted(entry);
    notify(kind === "conclusion" ? "结论已进入记忆" : "讨论过程已进入记忆");
  }

  function activate() {
    if (!canActivate()) {
      notify("请先停止当前回复");
      return;
    }
    active = true;
    render();
    notify("已打开独立讨论：可以读取记忆，但不会自动保存");
    focusInput();
  }

  toggle.addEventListener("click", activate);
  finish.addEventListener("click", () => {
    openDecision().catch((error) => notify(error.message));
  });
  backdrop.addEventListener("click", () => {
    openDecision().catch((error) => notify(error.message));
  });
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const message = input.value.trim();
    if (!message || busy || held) return;
    input.value = "";
    resizeInput();
    render();
    submitMessage(message);
  });
  input.addEventListener("input", () => {
    resizeInput();
    render();
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      form.requestSubmit();
    }
  });
  stopButton.addEventListener("click", interrupt);
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || !active || dialog.open) return;
    event.preventDefault();
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
  };
}
