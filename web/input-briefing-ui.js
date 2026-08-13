function observedTime(signal) {
  const value = signal.observedAt || signal.observed_at || new Date().toISOString();
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function relatedIds(signal) {
  return signal.relatedSignalIds || signal.related_signal_ids || [];
}

export function briefingRoleProjection(signals, roles) {
  const external = signals.filter((signal) => signal.kind !== "attacker_challenge");
  return roles
    .filter((role) => role.id !== "symbiont_attacker")
    .map((role) => ({
      ...role,
      count: external.filter((signal) => signal.actor?.id === role.id).length,
    }))
    .filter((role) => role.count > 0);
}

export function briefingEntries(signals, roleId) {
  const sourceIds = new Set(
    signals
      .filter((signal) => signal.kind !== "attacker_challenge" && signal.actor?.id === roleId)
      .map((signal) => signal.id),
  );
  return signals.filter(
    (signal) =>
      signal.actor?.id === roleId ||
      (signal.kind === "attacker_challenge" && relatedIds(signal).some((id) => sourceIds.has(id))),
  );
}

export function initInputBriefingUi({ state, renderMessageContent, applyAvatar, renderIcons, onReply }) {
  const dialog = document.querySelector("#input-briefing-dialog");
  const trigger = document.querySelector("#open-input-briefing");
  const roleList = document.querySelector("#input-briefing-roles");
  const feedHeader = document.querySelector("#input-briefing-feed-header");
  const content = document.querySelector("#input-briefing-content");
  let selectedRoleId = null;

  function selectDefault(roles) {
    if (roles.some((role) => role.id === selectedRoleId)) return;
    selectedRoleId = roles[0]?.id || null;
  }

  function signalCard(signal) {
    const card = document.createElement("article");
    card.className = "input-briefing-card";
    if (signal.kind === "attacker_challenge") card.classList.add("is-dissent");
    const avatar = document.createElement("div");
    avatar.className = "input-briefing-avatar input-role-avatar";
    applyAvatar(avatar, signal.actor?.avatarSeed || signal.actor?.avatar_seed);
    const main = document.createElement("div");
    main.className = "input-briefing-card-main";
    const meta = document.createElement("header");
    const author = document.createElement("strong");
    author.textContent = signal.actor?.name || "外部输入";
    const stamp = document.createElement("small");
    stamp.textContent = `${signal.kind === "attacker_challenge" ? "异议 · " : "外部输入 · "}${observedTime(signal)}`;
    meta.append(author, stamp);
    const body = document.createElement("div");
    body.className = "input-briefing-card-body";
    const text = signal.content || signal.receivedText || signal.received_text || signal.summary || signal.title || "";
    renderMessageContent(body, { content: text, parts: [{ type: "markdown", text }] });
    main.append(meta, body);
    if (signal.kind === "attacker_challenge" && relatedIds(signal).length) {
      const relation = document.createElement("small");
      relation.className = "input-briefing-relation";
      relation.textContent = `↳ 回应 ${relatedIds(signal).length} 条该角色输入`;
      main.append(relation);
    }
    const received = signal.receivedText || signal.received_text;
    if (signal.presentation === "condensed" && received && received.trim() !== text.trim()) {
      const original = document.createElement("details");
      original.className = "input-briefing-original";
      const summary = document.createElement("summary");
      summary.textContent = "展开收到的原文";
      const originalBody = document.createElement("div");
      renderMessageContent(originalBody, { content: received, parts: [{ type: "markdown", text: received }] });
      original.append(summary, originalBody);
      main.append(original);
    }
    if (signal.sources?.length) {
      const sources = document.createElement("details");
      sources.className = "input-briefing-sources";
      const summary = document.createElement("summary");
      summary.textContent = `${signal.sources.length} 个来源`;
      const list = document.createElement("ul");
      for (const source of signal.sources) {
        const item = document.createElement("li");
        const link = document.createElement("a");
        link.href = source.url;
        link.target = "_blank";
        link.rel = "noreferrer";
        link.textContent = source.detail || source.url;
        item.append(link);
        list.append(item);
      }
      sources.append(summary, list);
      main.append(sources);
    }
    const actions = document.createElement("footer");
    const reply = document.createElement("button");
    reply.type = "button";
    reply.className = "input-briefing-reply";
    reply.textContent = "在对话中回应";
    reply.addEventListener("click", () => onReply(signal));
    actions.append(reply);
    main.append(actions);
    card.append(avatar, main);
    return card;
  }

  function render() {
    const roles = briefingRoleProjection(state.signals || [], state.inputRoles?.roles || []);
    selectDefault(roles);
    trigger.hidden = roles.length === 0;
    if (!dialog || !roleList || !feedHeader || !content) return;
    roleList.replaceChildren();
    for (const role of roles) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "input-briefing-role";
      button.setAttribute("aria-pressed", String(role.id === selectedRoleId));
      const avatar = document.createElement("span");
      avatar.className = "input-briefing-role-avatar input-role-avatar";
      applyAvatar(avatar, role.avatar);
      const name = document.createElement("span");
      name.textContent = role.name;
      const count = document.createElement("small");
      count.textContent = String(role.count);
      button.append(avatar, name, count);
      button.addEventListener("click", () => {
        selectedRoleId = role.id;
        render();
      });
      roleList.append(button);
    }
    const selected = roles.find((role) => role.id === selectedRoleId);
    feedHeader.replaceChildren();
    content.replaceChildren();
    if (!selected) {
      feedHeader.textContent = "暂时没有可查看的外部输入。";
      return;
    }
    const heading = document.createElement("strong");
    heading.textContent = selected.name;
    const summary = document.createElement("small");
    summary.textContent = `${selected.count} 条外部输入 · 关联异议会显示在对应位置`;
    feedHeader.append(heading, summary);
    for (const signal of briefingEntries(state.signals || [], selected.id)) {
      content.append(signalCard(signal));
    }
    renderIcons(content);
  }

  trigger?.addEventListener("click", () => {
    render();
    if (!dialog.open) dialog.showModal();
  });

  return { render };
}
