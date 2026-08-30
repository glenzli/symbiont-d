export function modelMentionAt(text, caret) {
  const value = String(text || "");
  const end = Math.min(Math.max(caret ?? value.length, 0), value.length);
  const before = value.slice(0, end);
  let start = before.length;
  while (start > 0 && /[A-Za-z0-9_-]/.test(before[start - 1])) start -= 1;
  if (start === 0 || before[start - 1] !== "@") return null;
  const at = start - 1;
  if (at > 0 && /[A-Za-z0-9_@-]/.test(before[at - 1])) return null;
  return { start: at, end, query: before.slice(start).toLowerCase() };
}

export function mentionedModelIds(text, participants) {
  const enabled = new Set(
    (participants || []).filter((item) => item.enabled).map((item) => item.id),
  );
  const found = new Set();
  let fenced = false;
  for (const line of String(text || "").split("\n")) {
    const trimmed = line.trimStart();
    if (trimmed.startsWith("```") || trimmed.startsWith("~~~")) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    let inlineCode = false;
    for (let index = 0; index < line.length; index += 1) {
      if (line[index] === "\\") {
        index += 1;
        continue;
      }
      if (line[index] === "`") {
        inlineCode = !inlineCode;
        continue;
      }
      if (line[index] !== "@" || inlineCode) continue;
      if (index > 0 && /[A-Za-z0-9_@-]/.test(line[index - 1])) continue;
      let end = index + 1;
      while (end < line.length && /[A-Za-z0-9_-]/.test(line[end])) end += 1;
      const candidate = line.slice(index + 1, end);
      if (enabled.has(candidate)) found.add(candidate);
      index = Math.max(index, end - 1);
    }
  }
  return [...found].sort();
}

export function initModelCouncilUi({ state, notify, input, getTopic }) {
  const picker = document.querySelector(".model-council-picker");
  const toggle = document.querySelector("#model-council-toggle");
  const menu = document.querySelector("#model-council-menu");
  const options = document.querySelector("#model-council-options");
  const status = document.querySelector("#model-council-status");
  const count = document.querySelector("#model-council-count");
  const activeTray = document.querySelector("#model-council-active");
  const mentionMenu = document.querySelector("#model-mention-menu");
  const queued = new Set();
  const activations = new Map();
  let activationEpoch = 0;
  let mentionMatches = [];
  let mentionIndex = 0;

  function available() {
    return (state.modelCouncil?.participants || []).filter((item) => item.enabled);
  }

  function topic() {
    return getTopic?.() || null;
  }

  function scopeKey(current = topic()) {
    return current?.id ? `topic:${current.id}` : "main";
  }

  function currentActivation() {
    return activations.get(scopeKey()) || { scope: scopeKey(), participants: [] };
  }

  function activeIds() {
    return new Set(
      currentActivation().participants.map((participant) => participant.participantId),
    );
  }

  function maximum() {
    return state.modelCouncil?.maximumSelected || 3;
  }

  function render() {
    const participants = available();
    const active = activeIds();
    for (const id of [...queued]) {
      if (!participants.some((item) => item.id === id) || active.has(id)) queued.delete(id);
    }
    options.replaceChildren();
    if (!participants.length) {
      const empty = document.createElement("p");
      empty.textContent = "尚未配置可用的参与模型。";
      options.append(empty);
    }
    for (const participant of participants) {
      const label = document.createElement("label");
      const inputControl = document.createElement("input");
      const isActive = active.has(participant.id);
      inputControl.type = "checkbox";
      inputControl.checked = isActive || queued.has(participant.id);
      inputControl.disabled =
        !inputControl.checked && active.size + queued.size >= maximum();
      inputControl.addEventListener("change", async () => {
        if (isActive && !inputControl.checked) {
          await deactivate(participant.id);
          return;
        }
        if (inputControl.checked && active.size + queued.size >= maximum()) {
          inputControl.checked = false;
          notify?.(`每个主题最多激活 ${maximum()} 个模型`);
          return;
        }
        if (inputControl.checked) queued.add(participant.id);
        else queued.delete(participant.id);
        render();
      });
      const avatar = document.createElement("span");
      avatar.className = "model-council-option-avatar";
      avatar.textContent = participant.avatar || "◌";
      const text = document.createElement("span");
      const name = document.createElement("strong");
      name.textContent = participant.name;
      const detail = document.createElement("small");
      detail.textContent = isActive
        ? `@${participant.id} · 已加入当前主题`
        : `@${participant.id} · ${participant.role || participant.model}`;
      text.append(name, detail);
      label.append(inputControl, avatar, text);
      options.append(label);
    }
    const selectedCount = new Set([...active, ...queued]).size;
    count.hidden = selectedCount === 0;
    count.textContent = String(selectedCount);
    toggle.classList.toggle("active", selectedCount > 0);
    status.textContent = queued.size
      ? `发送后将激活 ${queued.size} 个模型；之后由人的消息唤醒。`
      : active.size
        ? `${active.size} 个模型已加入；可静默或自主离场。`
        : "输入 @模型ID，或在这里选择模型。";
    renderActiveTray();
  }

  function renderActiveTray() {
    const participants = currentActivation().participants;
    activeTray.replaceChildren();
    activeTray.hidden = participants.length === 0;
    for (const participant of participants) {
      const chip = document.createElement("span");
      chip.className = "model-council-active-chip";
      const avatar = document.createElement("span");
      avatar.textContent = participant.avatar || "◌";
      avatar.setAttribute("aria-hidden", "true");
      const name = document.createElement("strong");
      name.textContent = participant.name || participant.participantId;
      const stateLabel = document.createElement("small");
      stateLabel.textContent = "已加入";
      const leave = document.createElement("button");
      leave.type = "button";
      leave.textContent = "×";
      leave.title = `让 ${name.textContent} 离开当前主题`;
      leave.setAttribute("aria-label", leave.title);
      leave.addEventListener("click", () => deactivate(participant.participantId));
      chip.append(avatar, name, stateLabel, leave);
      activeTray.append(chip);
    }
  }

  async function refreshActivation() {
    const current = topic();
    const requestedScope = scopeKey(current);
    const epoch = ++activationEpoch;
    const query = current?.id ? `?topicId=${encodeURIComponent(current.id)}` : "";
    try {
      const response = await fetch(`/api/model-council/activation${query}`);
      const snapshot = await response.json();
      if (!response.ok) throw new Error(snapshot.error || "无法读取模型参与状态");
      if (epoch !== activationEpoch || requestedScope !== scopeKey()) return;
      applyActivation(snapshot);
    } catch (error) {
      if (epoch === activationEpoch) notify?.(error.message);
    }
  }

  async function deactivate(participantId) {
    const current = topic();
    try {
      const response = await fetch("/api/model-council/activation", {
        method: "DELETE",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ participantId, topicId: current?.id || null }),
      });
      const snapshot = await response.json();
      if (!response.ok) throw new Error(snapshot.error || "无法让模型离开");
      applyActivation(snapshot);
    } catch (error) {
      notify?.(error.message);
    }
  }

  function applyActivation(snapshot) {
    if (!snapshot?.scope) return;
    activations.set(snapshot.scope, snapshot);
    render();
  }

  function close() {
    menu.hidden = true;
    toggle.setAttribute("aria-expanded", "false");
  }

  function closeMentionMenu() {
    mentionMenu.hidden = true;
    mentionMenu.replaceChildren();
    mentionMatches = [];
    mentionIndex = 0;
  }

  function renderMentionMenu() {
    const mention = modelMentionAt(input.value, input.selectionStart ?? input.value.length);
    if (!mention) {
      closeMentionMenu();
      return;
    }
    mentionMatches = available()
      .filter(
        (participant) =>
          participant.id.toLowerCase().startsWith(mention.query) ||
          participant.name.toLowerCase().includes(mention.query),
      )
      .slice(0, 8);
    if (!mentionMatches.length) {
      closeMentionMenu();
      return;
    }
    mentionIndex = Math.min(mentionIndex, mentionMatches.length - 1);
    mentionMenu.replaceChildren();
    for (const [index, participant] of mentionMatches.entries()) {
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute("role", "option");
      button.setAttribute("aria-selected", String(index === mentionIndex));
      const name = document.createElement("strong");
      name.textContent = participant.name;
      const handle = document.createElement("small");
      handle.textContent = `@${participant.id}`;
      button.append(name, handle);
      button.addEventListener("mousedown", (event) => event.preventDefault());
      button.addEventListener("click", () => insertMention(participant));
      mentionMenu.append(button);
    }
    mentionMenu.hidden = false;
  }

  function insertMention(participant) {
    const caret = input.selectionStart ?? input.value.length;
    const mention = modelMentionAt(input.value, caret);
    if (!mention) return;
    const replacement = `@${participant.id} `;
    input.value =
      input.value.slice(0, mention.start) + replacement + input.value.slice(mention.end);
    const nextCaret = mention.start + replacement.length;
    input.setSelectionRange(nextCaret, nextCaret);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    closeMentionMenu();
    input.focus();
  }

  toggle.addEventListener("click", () => {
    const opening = menu.hidden;
    menu.hidden = !opening;
    toggle.setAttribute("aria-expanded", String(opening));
    if (opening) render();
  });
  document.addEventListener("click", (event) => {
    if (!picker.contains(event.target)) close();
  });
  input.addEventListener("input", renderMentionMenu);
  input.addEventListener("click", renderMentionMenu);
  input.addEventListener("blur", () => window.setTimeout(closeMentionMenu, 120));
  input.addEventListener("keydown", (event) => {
    if (mentionMenu.hidden || !mentionMatches.length) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      mentionIndex =
        (mentionIndex + (event.key === "ArrowDown" ? 1 : -1) + mentionMatches.length) %
        mentionMatches.length;
      renderMentionMenu();
    } else if (event.key === "Enter" || event.key === "Tab") {
      event.preventDefault();
      event.stopImmediatePropagation();
      insertMention(mentionMatches[mentionIndex]);
    } else if (event.key === "Escape") {
      event.preventDefault();
      closeMentionMenu();
    }
  });
  window.addEventListener("symbiont:model-council-updated", () => {
    render();
    refreshActivation();
  });

  return {
    applyActivation,
    refreshActivation,
    configUpdated() {
      render();
      refreshActivation();
    },
    prepare(text) {
      const ids = new Set([
        ...queued,
        ...mentionedModelIds(text, state.modelCouncil?.participants || []),
      ]);
      const combined = new Set([...activeIds(), ...ids]);
      if (combined.size > maximum()) {
        notify?.(`每个主题最多激活 ${maximum()} 个模型`);
        return null;
      }
      return [...ids];
    },
    commit(ids) {
      const combined = new Set([...activeIds(), ...(ids || [])]);
      if (ids?.length) {
        const participantById = new Map(
          available().map((participant) => [participant.id, participant]),
        );
        const participants = [...combined]
          .map((id) => {
            const configured = participantById.get(id);
            return configured
              ? { participantId: id, name: configured.name, avatar: configured.avatar }
              : currentActivation().participants.find((item) => item.participantId === id);
          })
          .filter(Boolean);
        applyActivation({
          scope: scopeKey(),
          topicId: topic()?.id || null,
          participants,
        });
      }
      queued.clear();
      close();
      render();
    },
    scopeChanged() {
      queued.clear();
      closeMentionMenu();
      render();
      refreshActivation();
    },
  };
}
