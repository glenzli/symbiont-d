import { formatDate, responseJson } from "/presentation.js";
import { renderMessageContent, renderRichText } from "/rich-text.js";

export function initTopicUi({ conversation, focusComposer }) {
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

    const summary = document.createElement("div");
    summary.className = "topic-summary";
    renderRichText(summary, topic.summary);

    const timeline = document.createElement("div");
    timeline.className = "topic-timeline";
    for (const message of payload.messages || []) {
      timeline.append(renderTopicMessage(message));
    }
    if (!timeline.childElementCount) {
      const empty = document.createElement("p");
      empty.className = "topic-timeline-empty";
      empty.textContent = "暂无可读取的原始消息";
      timeline.append(empty);
    }
    detail.append(header, summary, timeline);
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

function renderTopicMessage(message) {
  const item = document.createElement("article");
  item.className = "topic-message";
  item.dataset.role = message.role;
  const meta = document.createElement("header");
  const speaker = document.createElement("strong");
  speaker.textContent = message.role === "user" ? "你" : "symbiont-d";
  const time = document.createElement("time");
  time.dateTime = message.at;
  time.textContent = formatDate(message.at);
  meta.append(speaker, time);
  const body = document.createElement("div");
  body.className = "topic-message-body";
  renderMessageContent(body, message);
  item.append(meta, body);
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
