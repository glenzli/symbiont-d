import { formatDate, responseJson } from "/presentation.js";
import { renderMessageContent, renderRichText } from "/rich-text.js";

export function initTopicUi({ conversation, focusComposer, applyAvatar }) {
  const openButton = document.querySelector("#open-topics");
  const dialog = document.querySelector("#topic-dialog");
  const status = document.querySelector("#topic-dialog-status");
  const list = document.querySelector("#topic-list");
  const detail = document.querySelector("#topic-detail");
  const tray = document.querySelector("#topic-target-tray");
  const trayTitle = document.querySelector("#topic-target-title");
  const openSelected = document.querySelector("#open-selected-topic");
  const clearSelected = document.querySelector("#clear-topic-target");

  let topics = [];
  let selectedTopicId = null;
  let pendingTopic = null;
  let activeDetail = null;
  let selectedView = "current";
  let loadEpoch = 0;

  openButton.addEventListener("click", () => open());
  openSelected.addEventListener("click", () => {
    if (pendingTopic) open(pendingTopic.id);
  });
  clearSelected.addEventListener("click", () => clear());
  list.addEventListener("click", (event) => {
    const button = event.target.closest("[data-topic-id]");
    if (button) select(button.dataset.topicId);
  });
  detail.addEventListener("click", (event) => {
    const reference = event.target.closest("[data-message-topic-id]");
    if (reference) {
      open(reference.dataset.messageTopicId);
      return;
    }
    const related = event.target.closest("[data-related-topic]");
    if (related) {
      select(related.dataset.relatedTopic);
      return;
    }
    const view = event.target.closest("[data-topic-view]");
    if (view && activeDetail) {
      selectedView = view.dataset.topicView;
      renderDetail(activeDetail);
      return;
    }
    const expand = event.target.closest("[data-topic-expand]");
    if (expand) {
      const content = expand.closest(".topic-evolution-message");
      content?.classList.toggle("expanded");
      expand.textContent = content?.classList.contains("expanded")
        ? "收起"
        : "展开原文";
      expand.setAttribute(
        "aria-expanded",
        String(content?.classList.contains("expanded")),
      );
      return;
    }
    const button = event.target.closest("[data-continue-topic]");
    if (!button) return;
    const item = topics.find(({ topic }) => topic.id === button.dataset.continueTopic);
    if (!item) return;
    set({ id: item.topic.id, title: item.topic.title });
    dialog.close();
    focusComposer();
  });
  conversation.addEventListener("click", (event) => {
    const reference = event.target.closest("[data-message-topic-id]");
    if (reference) open(reference.dataset.messageTopicId);
  });

  async function open(preferredId = null) {
    if (!dialog.open) dialog.showModal();
    await load(preferredId || selectedTopicId || pendingTopic?.id);
  }

  async function load(preferredId) {
    const epoch = ++loadEpoch;
    status.textContent = "正在读取";
    list.textContent = "";
    renderEmpty("正在读取主题");
    try {
      const payload = await responseJson(await fetch("/api/topics"), "无法读取主题");
      if (epoch !== loadEpoch) return;
      topics = payload.topics || [];
      status.textContent = topics.length ? `${topics.length} 个主题` : "尚未形成主题";
      renderList();
      const nextId = topics.some(({ topic }) => topic.id === preferredId)
        ? preferredId
        : topics[0]?.topic.id;
      if (nextId) await select(nextId, epoch);
      else renderEmpty("尚未形成主题");
    } catch (error) {
      status.textContent = error.message;
      renderEmpty(error.message);
    }
  }

  function renderList() {
    list.replaceChildren();
    for (const item of topics) {
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.topicId = item.topic.id;
      button.className = "topic-list-item";
      button.classList.toggle("selected", item.topic.id === selectedTopicId);
      const heading = document.createElement("strong");
      heading.textContent = item.topic.title;
      const meta = document.createElement("small");
      meta.textContent = `${topicState(item.topic.state)} · ${item.messageCount} 条`;
      button.append(heading, meta);
      list.append(button);
    }
  }

  async function select(topicId, parentEpoch = loadEpoch) {
    selectedTopicId = topicId;
    selectedView = "current";
    activeDetail = null;
    renderList();
    renderEmpty("正在读取记录");
    try {
      const payload = await responseJson(
        await fetch(`/api/topics/${encodeURIComponent(topicId)}`),
        "无法读取主题记录",
      );
      if (parentEpoch !== loadEpoch || selectedTopicId !== topicId) return;
      renderDetail(payload);
    } catch (error) {
      if (selectedTopicId === topicId) renderEmpty(error.message);
    }
  }

  function renderDetail(payload) {
    activeDetail = payload;
    detail.replaceChildren();
    const topic = payload.topic;
    const header = document.createElement("header");
    header.className = "topic-detail-header";
    const heading = document.createElement("div");
    const title = document.createElement("h3");
    title.textContent = topic.title;
    const meta = document.createElement("p");
    meta.textContent = `${topicState(topic.state)} · ${payload.messageCount} 条记录 · ${formatDate(topic.lastActivityAt)}`;
    heading.append(title, meta);
    const continueButton = document.createElement("button");
    continueButton.type = "button";
    continueButton.className = "secondary-button";
    continueButton.dataset.continueTopic = topic.id;
    continueButton.textContent = "从这里继续";
    header.append(heading, continueButton);
    const topicLookup = new Map(topics.map(({ topic: item }) => [item.id, item]));
    detail.append(header, renderViewTabs(selectedView));
    if (selectedView === "evolution") {
      detail.append(renderEvolution(payload));
    } else if (selectedView === "evidence") {
      detail.append(renderEvidence(payload, applyAvatar));
    } else {
      detail.append(renderCurrentLens(payload, topicLookup));
    }
  }

  function set(topic) {
    if (!topic?.id || !topic?.title) return;
    pendingTopic = { id: topic.id, title: topic.title };
    trayTitle.textContent = topic.title;
    tray.hidden = false;
  }

  function clear() {
    pendingTopic = null;
    trayTitle.textContent = "";
    tray.hidden = true;
  }

  function consume() {
    const topic = pendingTopic;
    clear();
    return topic;
  }

  function renderEmpty(text) {
    detail.replaceChildren();
    const empty = document.createElement("div");
    empty.className = "topic-empty";
    empty.textContent = text;
    detail.append(empty);
  }

  return { clear, consume, open, set };
}

function renderViewTabs(selectedView) {
  const tabs = document.createElement("nav");
  tabs.className = "topic-view-tabs";
  tabs.setAttribute("aria-label", "主题视图");
  for (const [id, label] of [
    ["current", "当前理解"],
    ["evolution", "演化"],
    ["evidence", "证据"],
  ]) {
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.topicView = id;
    button.textContent = label;
    button.setAttribute("aria-pressed", String(selectedView === id));
    tabs.append(button);
  }
  return tabs;
}

function renderCurrentLens(payload, topicLookup) {
  const section = document.createElement("section");
  section.className = "topic-lens";
  const understanding = document.createElement("section");
  understanding.className = "topic-lens-card topic-current-understanding";
  appendLensHeading(understanding, "当前理解", stateDescription(payload.topic.state));
  const summary = document.createElement("div");
  summary.className = "topic-summary";
  renderRichText(summary, payload.topic.summary);
  understanding.append(summary);
  section.append(understanding);

  const latestUser = [...(payload.messages || [])]
    .reverse()
    .find((message) => message.role === "user");
  const latestAssistant = [...(payload.messages || [])]
    .reverse()
    .find((message) => message.role === "assistant");
  if (latestUser || latestAssistant) {
    const landing = document.createElement("section");
    landing.className = "topic-lens-landings";
    if (latestUser) {
      landing.append(renderLanding("你的最近推进", latestUser));
    }
    if (latestAssistant) {
      landing.append(renderLanding("当前回应", latestAssistant));
    }
    section.append(landing);
  }

  const relatedTopics = (payload.topic.parentEpisodeIds || [])
    .map((id) => topicLookup.get(id))
    .filter(Boolean);
  if (relatedTopics.length) {
    const relations = document.createElement("section");
    relations.className = "topic-lens-card topic-relations";
    appendLensHeading(relations, "相关主题", "只显示明确的来源关系");
    const chips = document.createElement("div");
    chips.className = "topic-relation-chips";
    for (const related of relatedTopics) {
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.relatedTopic = related.id;
      button.textContent = related.title;
      chips.append(button);
    }
    relations.append(chips);
    section.append(relations);
  }
  return section;
}

function renderEvolution(payload) {
  const section = document.createElement("section");
  section.className = "topic-evolution";
  const introduction = document.createElement("p");
  introduction.className = "topic-view-introduction";
  introduction.textContent = "按你的推进与随后形成的回应整理；完整可追溯内容在“证据”。";
  section.append(introduction);
  const moves = topicMoves(payload.messages || []);
  if (!moves.length) {
    section.append(topicEmpty("暂无可整理的主题推进。"));
    return section;
  }
  for (const [index, move] of moves.entries()) {
    const step = document.createElement("article");
    step.className = "topic-evolution-step";
    step.dataset.step = String(index + 1);
    step.dataset.hasNext = String(index < moves.length - 1);
    const header = document.createElement("header");
    const title = document.createElement("strong");
    title.textContent = move.user ? `第 ${index + 1} 次推进` : "补充回应";
    const time = document.createElement("time");
    time.dateTime = (move.user || move.assistants[0])?.at || "";
    time.textContent = formatDate(time.dateTime);
    header.append(title, time);
    step.append(header);
    if (move.user) step.append(renderEvolutionMessage("你的推进", move.user));
    if (move.user && move.assistants.length) {
      step.append(renderEvolutionTransition("形成回应"));
    }
    for (const message of move.assistants) {
      step.append(renderEvolutionMessage("形成的回应", message));
    }
    section.append(step);
    if (index < moves.length - 1) {
      section.append(renderEvolutionConnector("继续推进"));
    }
  }
  return section;
}

function renderEvidence(payload, applyAvatar) {
  const section = document.createElement("section");
  section.className = "topic-evidence";
  const introduction = document.createElement("p");
  introduction.className = "topic-view-introduction";
  introduction.textContent = "主题所依据的原始对话记录。";
  const timeline = document.createElement("div");
  timeline.className = "topic-timeline";
  for (const message of payload.messages || []) {
    timeline.append(renderTopicMessage(message, applyAvatar));
  }
  if (!timeline.childElementCount) timeline.append(topicEmpty("暂无可读取的原始消息。"));
  section.append(introduction, timeline);
  return section;
}

function appendLensHeading(parent, title, note) {
  const header = document.createElement("header");
  const heading = document.createElement("strong");
  heading.textContent = title;
  const copy = document.createElement("span");
  copy.textContent = note;
  header.append(heading, copy);
  parent.append(header);
}

function renderLanding(label, message) {
  const card = document.createElement("section");
  card.className = `topic-lens-card topic-landing topic-landing-${message.role}`;
  appendLensHeading(card, label, formatDate(message.at));
  const body = document.createElement("div");
  body.className = "topic-landing-body";
  renderMessageContent(body, message);
  card.append(body);
  return card;
}

function renderEvolutionMessage(label, message) {
  const item = document.createElement("section");
  item.className = `topic-evolution-message topic-evolution-${message.role}`;
  const header = document.createElement("header");
  const heading = document.createElement("strong");
  heading.textContent = label;
  const expand = document.createElement("button");
  expand.type = "button";
  expand.dataset.topicExpand = "";
  expand.setAttribute("aria-expanded", "false");
  expand.textContent = "展开原文";
  header.append(heading, expand);
  const body = document.createElement("div");
  body.className = "topic-evolution-body";
  renderMessageContent(body, message);
  item.append(header, body);
  return item;
}

function renderEvolutionTransition(label) {
  const transition = document.createElement("div");
  transition.className = "topic-evolution-transition";
  transition.setAttribute("aria-label", label);
  const arrow = document.createElement("span");
  arrow.setAttribute("aria-hidden", "true");
  arrow.textContent = "↓";
  const copy = document.createElement("small");
  copy.textContent = label;
  transition.append(arrow, copy);
  return transition;
}

function renderEvolutionConnector(label) {
  const connector = document.createElement("div");
  connector.className = "topic-evolution-connector";
  connector.setAttribute("aria-label", label);
  const arrow = document.createElement("span");
  arrow.setAttribute("aria-hidden", "true");
  arrow.textContent = "↓";
  const copy = document.createElement("small");
  copy.textContent = label;
  connector.append(arrow, copy);
  return connector;
}

function topicMoves(messages) {
  const moves = [];
  let current = null;
  for (const message of messages) {
    if (message.role === "user") {
      current = { user: message, assistants: [] };
      moves.push(current);
    } else if (current) {
      current.assistants.push(message);
    } else {
      moves.push({ user: null, assistants: [message] });
    }
  }
  return moves;
}

function topicEmpty(text) {
  const empty = document.createElement("p");
  empty.className = "topic-timeline-empty";
  empty.textContent = text;
  return empty;
}

function stateDescription(state) {
  return (
    {
      forming: "仍在形成，可继续推进",
      active: "当前可从这里继续",
      dormant: "暂未继续，但仍可恢复",
      closed: "已形成阶段性结论",
    }[state] || "主题当前状态"
  );
}

function renderTopicMessage(message, applyAvatar) {
  const item = document.createElement("article");
  item.className = "topic-chat-message";
  item.dataset.role = message.role;
  const layout = document.createElement("div");
  layout.className = "topic-chat-layout";
  const avatar = document.createElement("div");
  avatar.className = "message-avatar topic-chat-avatar";
  avatar.setAttribute("aria-hidden", "true");
  const image = document.createElement("img");
  image.className = "message-avatar-image";
  image.src = "/symbiont-avatar.png?v=icon-20260808";
  image.alt = "";
  const fallback = document.createElement("span");
  fallback.className = "message-avatar-fallback";
  fallback.hidden = true;
  fallback.textContent = "你";
  avatar.append(image, fallback);
  applyAvatar?.(avatar, message.role === "user" ? "user" : "symbiont");
  const content = document.createElement("div");
  content.className = "topic-chat-content";
  const meta = document.createElement("header");
  meta.className = "topic-chat-meta";
  const speaker = document.createElement("strong");
  speaker.textContent = message.role === "user" ? "你" : "symbiont-d";
  const time = document.createElement("time");
  time.dateTime = message.at;
  time.textContent = formatDate(message.at);
  meta.append(speaker, time);
  const body = document.createElement("div");
  body.className = "topic-chat-body";
  renderMessageContent(body, message);
  content.append(meta, body);
  layout.append(avatar, content);
  item.append(layout);
  return item;
}

function topicState(value) {
  return (
    {
      forming: "形成中",
      active: "活跃",
      dormant: "沉寂",
      closed: "结束",
    }[value] || value
  );
}
