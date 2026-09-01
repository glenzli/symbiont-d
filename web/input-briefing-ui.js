import { annotationsBySource, attachAnnotations } from "./input-signal-relations.js";
import { signalContent, appendSignalDetails } from "./input-signal-content.js";

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

function localDateKey(date) {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function observedDate(signal, fallback = new Date()) {
  const value = signal.observedAt || signal.observed_at;
  const date = value ? new Date(value) : fallback;
  return Number.isNaN(date.getTime()) ? fallback : date;
}

export function briefingDateKey(signal, fallback) {
  return localDateKey(observedDate(signal, fallback));
}

export function todayBriefingDate() {
  return localDateKey(new Date());
}

export function formatBriefingDate(dateKey) {
  const [year, month, day] = dateKey.split("-").map(Number);
  const date = new Date(year, month - 1, day);
  if (Number.isNaN(date.getTime())) return dateKey;
  const label = date.toLocaleDateString([], { month: "long", day: "numeric" });
  return dateKey === todayBriefingDate() ? `今天 · ${label}` : label;
}

function shiftDate(dateKey, offset) {
  const [year, month, day] = dateKey.split("-").map(Number);
  const date = new Date(year, month - 1, day, 12);
  date.setDate(date.getDate() + offset);
  return localDateKey(date);
}

function relatedIds(signal) {
  return signal.relatedSignalIds || signal.related_signal_ids || [];
}

function chronologicalSignals(signals) {
  return signals
    .map((signal, index) => ({ signal, index }))
    .sort(
      (left, right) =>
        observedDate(left.signal).getTime() - observedDate(right.signal).getTime() ||
        left.index - right.index,
    )
    .map(({ signal }) => signal);
}

export function briefingRoleProjection(signals, roles, dateKey = null) {
  const external = signals.filter(
    (signal) =>
      signal.kind !== "attacker_challenge" &&
      (!dateKey || briefingDateKey(signal) === dateKey),
  );
  return roles
    .filter((role) => role.id !== "symbiont_attacker")
    .map((role) => ({
      ...role,
      count: external.filter((signal) => signal.actor?.id === role.id).length,
    }))
    .filter((role) => role.count > 0);
}

function topicOf(signal) {
  const topic = signal.briefingTopic || signal.briefing_topic;
  return topic && topic.trim() ? topic.trim() : "未归类";
}

export function briefingTopicStatus(signal) {
  const status = signal.briefingTopicStatus || signal.briefing_topic_status;
  if (status) return status;
  return topicOf(signal) === "未归类" ? "unclassified" : "classified";
}

function briefingTopicStatusCounts(signals, dateKey) {
  return signals.reduce((counts, signal) => {
    if (signal.kind === "attacker_challenge" || briefingDateKey(signal) !== dateKey) return counts;
    const status = briefingTopicStatus(signal);
    counts[status] = (counts[status] || 0) + 1;
    return counts;
  }, {});
}

export function briefingTopicProjection(signals, dateKey = null) {
  const counts = new Map();
  for (const signal of signals) {
    if (signal.kind === "attacker_challenge" || (dateKey && briefingDateKey(signal) !== dateKey)) continue;
    const topic = topicOf(signal);
    counts.set(topic, (counts.get(topic) || 0) + 1);
  }
  return [...counts]
    .map(([topic, count]) => ({ id: topic, name: topic, count }))
    .sort((left, right) =>
      (left.id === "未归类") - (right.id === "未归类") ||
      right.count - left.count ||
      left.name.localeCompare(right.name),
    );
}

function briefingEntriesForSources(signals, sources) {
  return chronologicalSignals(sources);
}

export function briefingEntries(signals, roleId, dateKey = null) {
  const sources = signals.filter(
    (signal) =>
      signal.kind !== "attacker_challenge" &&
      signal.actor?.id === roleId &&
      (!dateKey || briefingDateKey(signal) === dateKey),
  );
  return briefingEntriesForSources(signals, sources);
}

export function briefingTopicEntries(signals, topicId, dateKey = null) {
  const sources = signals.filter(
    (signal) =>
      signal.kind !== "attacker_challenge" &&
      (!dateKey || briefingDateKey(signal) === dateKey) &&
      (topicId === "__all__" || topicOf(signal) === topicId),
  );
  return briefingEntriesForSources(signals, sources);
}

export function briefingTopicRunNotice(result) {
  const queuedCount = Number(result?.queuedCount || result?.queued_count || 0);
  const assignedCount = Number(result?.assignedCount || result?.assigned_count || 0);
  switch (result?.outcome) {
    case "completed":
      return {
        problem: false,
        text: `${result?.reclassified ? "已重新整理" : "已整理"} ${queuedCount} 条：${assignedCount} 条归入主题，其余保留为未归类。`,
      };
    case "nothing_to_do":
      return { problem: false, text: "当天没有尚未整理的未归类输入。" };
    case "interrupted":
      return { problem: true, text: "整理被新的对话输入打断，尚未整理的内容仍可重试。" };
    case "deferred":
      return {
        problem: true,
        text: "本地模型没有返回可识别的分类结果，尚未整理的内容仍可重试。",
      };
    default:
      return { problem: true, text: "本地主题整理未完成，尚未整理的内容仍可重试。" };
  }
}

export function initInputBriefingUi({ state, renderMessageContent, applyAvatar, renderIcons, onReply, refreshRuntime = () => {}, notify = () => {} }) {
  const dialog = document.querySelector("#input-briefing-dialog");
  const trigger = document.querySelector("#open-input-briefing");
  const roleList = document.querySelector("#input-briefing-roles");
  const dateInput = document.querySelector("#input-briefing-date");
  const previousDate = document.querySelector("#input-briefing-previous-date");
  const nextDate = document.querySelector("#input-briefing-next-date");
  const rolesView = document.querySelector("#input-briefing-view-roles");
  const topicsView = document.querySelector("#input-briefing-view-topics");
  const topicActions = document.querySelector("#input-briefing-topic-actions");
  const organizeDate = document.querySelector("#input-briefing-organize-date");
  const reclassifyDate = document.querySelector("#input-briefing-reclassify-date");
  const organizeStatus = document.querySelector("#input-briefing-organize-status");
  const feedHeader = document.querySelector("#input-briefing-feed-header");
  const content = document.querySelector("#input-briefing-content");
  let selectedRoleId = null;
  let selectedTopicId = "__all__";
  let selectedView = "roles";
  let organizingDate = false;
  let organizeNotice = null;
  let selectedDate = todayBriefingDate();

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
    const topicState = briefingTopicStatus(signal);
    const topicNotice = signal.kind === "attacker_challenge"
      ? ""
      : topicState === "pending"
        ? " · 主题待本地整理"
        : topicState === "unavailable"
          ? " · 本地整理暂不可用"
          : "";
    stamp.textContent = `${signal.kind === "attacker_challenge" ? "异议 · " : "外部输入 · "}${observedTime(signal)}${topicNotice}`;
    meta.append(author, stamp);
    const body = document.createElement("div");
    body.className = "input-briefing-card-body";
    const text = signalContent(signal).text;
    renderMessageContent(body, { content: text, parts: [{ type: "markdown", text }] });
    main.append(meta, body);
    if (signal.kind === "attacker_challenge" && relatedIds(signal).length) {
      const relation = document.createElement("small");
      relation.className = "input-briefing-relation";
      relation.textContent = `↳ 回应 ${relatedIds(signal).length} 条该角色输入`;
      main.append(relation);
    }
    appendSignalDetails(main, signal, renderMessageContent);
    const actions = document.createElement("footer");
    const reply = document.createElement("button");
    reply.type = "button";
    reply.className = "input-briefing-reply";
    reply.textContent = "在对话中回应";
    reply.addEventListener("click", () => onReply(signal));
    actions.append(reply);
    main.append(actions);
    card.append(avatar, main);
    attachAnnotations(card, annotationsBySource(state.signals || []).get(signal.id) || [], renderMessageContent, { body, foot: actions });
    return card;
  }

  function render() {
    const allRoles = briefingRoleProjection(state.signals || [], state.inputRoles?.roles || []);
    const roles = briefingRoleProjection(
      state.signals || [],
      state.inputRoles?.roles || [],
      selectedDate,
    );
    selectDefault(roles);
    const topics = briefingTopicProjection(state.signals || [], selectedDate);
    const topicStatus = briefingTopicStatusCounts(state.signals || [], selectedDate);
    const allInputCount = topics.reduce((total, topic) => total + topic.count, 0);
    const unreviewedCount = (state.signals || []).filter((signal) =>
      signal.kind !== "attacker_challenge" &&
      briefingDateKey(signal) === selectedDate &&
      topicOf(signal) === "未归类" &&
      briefingTopicStatus(signal) !== "pending" &&
      !signal.briefingTopicReviewed &&
      !signal.briefing_topic_reviewed,
    ).length;
    trigger.hidden = allRoles.length === 0 && allInputCount === 0;
    if (!dialog || !roleList || !feedHeader || !content) return;
    if (dateInput) {
      dateInput.value = selectedDate;
      dateInput.max = todayBriefingDate();
    }
    if (nextDate) nextDate.disabled = selectedDate >= todayBriefingDate();
    rolesView?.setAttribute("aria-pressed", String(selectedView === "roles"));
    topicsView?.setAttribute("aria-pressed", String(selectedView === "topics"));
    if (topicActions) topicActions.hidden = selectedView !== "topics" || allInputCount === 0;
    if (organizeDate) {
      organizeDate.hidden = unreviewedCount === 0;
      organizeDate.disabled = organizingDate || unreviewedCount === 0;
      organizeDate.title = organizingDate
        ? "正在本地整理…"
        : `整理当天 ${unreviewedCount} 条未归类输入`;
      organizeDate.setAttribute("aria-label", organizeDate.title);
    }
    if (reclassifyDate) {
      reclassifyDate.disabled = organizingDate || allInputCount === 0;
      reclassifyDate.title = organizingDate
        ? "正在本地整理…"
        : `重新整理当天全部 ${allInputCount} 条输入（覆盖当前主题）`;
      reclassifyDate.setAttribute("aria-label", reclassifyDate.title);
    }
    if (organizeStatus) {
      organizeStatus.hidden = selectedView !== "topics" || !organizeNotice;
      organizeStatus.textContent = organizeNotice?.text || "";
      organizeStatus.classList.toggle("is-problem", Boolean(organizeNotice?.problem));
    }
    roleList.setAttribute("aria-label", selectedView === "roles" ? "外部输入角色" : "输入主题");
    roleList.replaceChildren();
    const railItems = selectedView === "roles"
      ? roles.map((role) => ({ ...role, selected: role.id === selectedRoleId, kind: "role" }))
      : [
        { id: "__all__", name: "全部", count: allInputCount, selected: selectedTopicId === "__all__", kind: "topic" },
        ...topics.map((topic) => ({ ...topic, selected: topic.id === selectedTopicId, kind: "topic" })),
      ];
    if (selectedView === "topics" && !railItems.some((item) => item.id === selectedTopicId)) {
      selectedTopicId = "__all__";
    }
    for (const item of railItems) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "input-briefing-role";
      button.setAttribute("aria-pressed", String(item.selected));
      const avatar = document.createElement("span");
      avatar.className = "input-briefing-role-avatar input-role-avatar";
      if (item.kind === "role") applyAvatar(avatar, item.avatar);
      else avatar.textContent = item.id === "__all__" ? "●" : "#";
      const name = document.createElement("span");
      name.textContent = item.name;
      const count = document.createElement("small");
      count.textContent = String(item.count);
      button.append(avatar, name, count);
      button.addEventListener("click", () => {
        if (item.kind === "role") selectedRoleId = item.id;
        else selectedTopicId = item.id;
        render();
      });
      roleList.append(button);
    }
    feedHeader.replaceChildren();
    content.replaceChildren();
    const selected = selectedView === "roles"
      ? roles.find((role) => role.id === selectedRoleId)
      : railItems.find((item) => item.id === selectedTopicId);
    if (!selected || (selectedView === "topics" && allInputCount === 0)) {
      const heading = document.createElement("strong");
      heading.textContent = formatBriefingDate(selectedDate);
      const summary = document.createElement("small");
      summary.textContent = "当天没有可查看的外部输入。";
      feedHeader.append(heading, summary);
      return;
    }
    const heading = document.createElement("strong");
    heading.textContent = `${selected.name} · ${formatBriefingDate(selectedDate)}`;
    const summary = document.createElement("small");
    if (selectedView === "topics") {
      const statusNotice = topicStatus.pending
        ? ` · ${topicStatus.pending} 条主题待本地整理，暂显示在未归类`
        : topicStatus.unavailable
          ? ` · ${topicStatus.unavailable} 条本地整理暂不可用，留在未归类`
          : "";
      summary.textContent = `${selected.count} 条外部输入 · 主题仅用于浏览，审阅提示附着在原文上${statusNotice}`;
    } else {
      summary.textContent = `${selected.count} 条外部输入 · 按采集时间排列 · 审阅提示附着在原文上`;
    }
    feedHeader.append(heading, summary);
    const entries = selectedView === "topics"
      ? briefingTopicEntries(state.signals || [], selected.id, selectedDate)
      : briefingEntries(state.signals || [], selected.id, selectedDate);
    for (const signal of entries) {
      content.append(signalCard(signal));
    }
    renderIcons(content);
  }

  trigger?.addEventListener("click", () => {
    render();
    if (!dialog.open) dialog.showModal();
  });
  dateInput?.addEventListener("change", () => {
    if (!dateInput.value) return;
    selectedDate = dateInput.value;
    organizeNotice = null;
    render();
  });
  previousDate?.addEventListener("click", () => {
    selectedDate = shiftDate(selectedDate, -1);
    organizeNotice = null;
    render();
  });
  nextDate?.addEventListener("click", () => {
    const today = todayBriefingDate();
    if (selectedDate >= today) return;
    selectedDate = shiftDate(selectedDate, 1);
    organizeNotice = null;
    render();
  });
  rolesView?.addEventListener("click", () => {
    selectedView = "roles";
    render();
  });
  topicsView?.addEventListener("click", () => {
    selectedView = "topics";
    render();
  });
  async function organizeSelectedDate(reclassify) {
    if (organizingDate || !selectedDate) return;
    organizingDate = true;
    organizeNotice = null;
    render();
    try {
      const response = await fetch("/api/briefing/topics", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ date: selectedDate, reclassify }),
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || "无法整理当天输入主题");
      await refreshRuntime();
      organizeNotice = briefingTopicRunNotice(result);
      if (organizeNotice.problem) notify(organizeNotice.text);
    } catch (error) {
      organizeNotice = { problem: true, text: error.message || "无法整理当天输入主题" };
      notify(organizeNotice.text);
    } finally {
      organizingDate = false;
      render();
    }
  }

  organizeDate?.addEventListener("click", () => organizeSelectedDate(false));
  reclassifyDate?.addEventListener("click", () => organizeSelectedDate(true));

  return { render };
}
