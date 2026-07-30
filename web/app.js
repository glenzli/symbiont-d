import { initProfileUi } from "/profile-ui.js";
import { initReflectionUi } from "/reflection-ui.js";
import { formatDuration, formatMemorySize, formatTokens } from "/presentation.js";
import { renderMessageContent, renderRichText } from "/rich-text.js";
import { initExplorationUi } from "/exploration-ui.js";
import { initMessageActions } from "/message-actions.js";
import { initMessageSync } from "/message-sync.js";
import { initSettings } from "/settings.js";
import { initTaskUi } from "/task-ui.js";
import { initTraceUi } from "/trace-ui.js";

const appState = {
  models: [],
  compute: null,
  profile: { status: "unconfigured", mode: null, orientation: "" },
  autonomy: null,
  autonomyPermitted: false,
  usage: {
    totalTokens: 0,
    autonomousTokensToday: 0,
    autonomousMessagesToday: 0,
    reflectionTokensToday: 0,
  },
  exploration: null,
  reflection: null,
  conversation: null,
  bridge: { codexTaskAccess: false },
};

const conversation = document.querySelector("#conversation");
const emptyState = document.querySelector("#empty-state");
const composer = document.querySelector("#composer");
const input = document.querySelector("#message");
const sendButton = document.querySelector("#send");
const addImageButton = document.querySelector("#add-image");
const imageInput = document.querySelector("#image-input");
const attachmentTray = document.querySelector("#attachment-tray");
const composerState = document.querySelector("#composer-state");
const connectionStatus = document.querySelector("#connection-status");
const memorySize = document.querySelector("#memory-size");
const tokenTotal = document.querySelector("#token-total");
const messageTemplate = document.querySelector("#message-template");

let busy = false;
let activityStartedAt = 0;
let activityTimer = null;
let selectedImages = [];
let activeOutgoing = [];
let activePending = null;
let typingSignalTimer = null;

const MAX_IMAGES = 4;
const MAX_IMAGE_BYTES = 15 * 1024 * 1024;

const settingsUi = initSettings(appState);
initTaskUi(
  appState,
  (text) => {
    input.value = text;
    resizeComposer();
    input.focus();
  },
  settingsUi.open,
);
const explorationUi = initExplorationUi(appState);
const reflectionUi = initReflectionUi(appState);
const profileUi = initProfileUi(appState, sendMessage);
initTraceUi();
const messageSync = initMessageSync({
  conversation,
  appendMessage,
  applyRuntime,
  shouldDeferMessages: () => busy,
});
const messageActions = initMessageActions({
  conversation,
  isBusy: () => busy,
  perform: performMessageAction,
});

function metadataText(metadata) {
  if (!metadata?.runs?.length || appState.compute?.showModel === false) return "";
  const runs = metadata.runs.map((run) => {
    const name = run.displayName || run.model;
    return `${name} · ${run.effort}`;
  });
  return [
    metadata.origin === "autonomous" ? "主动探索" : "",
    runs.join(" → "),
    formatDuration(metadata.durationMs || 0),
    formatTokens(metadata.totalTokens || 0),
  ].filter(Boolean).join(" · ");
}

function renderMessageFoot(message, metadata) {
  const foot = message.querySelector(".message-foot");
  const runtime = foot.querySelector(".message-runtime");
  const traceButton = foot.querySelector(".trace-button");
  runtime.textContent = metadataText(metadata);
  traceButton.hidden = !metadata?.traceId;
  traceButton.dataset.traceId = metadata?.traceId || "";
  foot.hidden =
    !runtime.textContent &&
    traceButton.hidden &&
    !foot.querySelector(".message-state")?.textContent &&
    !foot.querySelector(".message-actions")?.childElementCount;
}

function appendMessage(entry, options = {}) {
  emptyState.hidden = true;
  const fragment = messageTemplate.content.cloneNode(true);
  const article = fragment.querySelector(".message");
  const speaker = fragment.querySelector(".speaker");
  const time = fragment.querySelector("time");
  const body = fragment.querySelector(".message-body");
  const role = entry.role || "assistant";

  article.dataset.role = role;
  if (entry.revisionId) article.dataset.revisionId = entry.revisionId;
  if (options.pending) article.classList.add("pending");
  if (options.error) article.classList.add("error");
  speaker.textContent = role === "user" ? "你" : "symbiont-d";
  time.dateTime = entry.at || new Date().toISOString();
  time.textContent = new Date(time.dateTime).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  renderMessageContent(body, entry);
  renderMessageFoot(article, entry.metadata);
  conversation.append(fragment);
  const element = conversation.lastElementChild;
  messageSync.track(element, entry, options);
  messageActions.track(element, entry, {
    deliveryState: options.deliveryState,
    failureReason: options.failureReason,
  });
  if (options.scroll !== false) {
    conversation.scrollTop = conversation.scrollHeight;
  }
  return element;
}

function resizeComposer() {
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 140)}px`;
}

function setBusy(nextBusy) {
  busy = nextBusy;
  composer.classList.toggle("response-active", nextBusy);
  messageActions.refresh();
  if (!nextBusy) {
    renderRuntimeStatus();
    clearInterval(activityTimer);
    activityTimer = null;
  }
}

function setActivity(message, event) {
  const body = message.querySelector(".message-body");
  const foot = message.querySelector(".message-foot");
  const runtime = foot.querySelector(".message-runtime");
  message.classList.add("pending");
  body.textContent = event.label;
  connectionStatus.textContent = event.label;
  foot.hidden = false;

  const refresh = () => {
    const elapsed = Date.now() - activityStartedAt;
    runtime.textContent = [
      event.displayName || event.model,
      event.effort,
      formatDuration(elapsed),
    ].join(" · ");
  };
  refresh();
  clearInterval(activityTimer);
  activityTimer = setInterval(refresh, 1000);
}

function applyAccepted(message, entry) {
  if (entry.revisionId) message.dataset.revisionId = entry.revisionId;
  renderMessageContent(message.querySelector(".message-body"), entry);
  messageSync.track(message, entry, { interactive: true });
  messageActions.update(message, entry, { deliveryState: "pending" });
}

function applyComplete(message, entry) {
  clearInterval(activityTimer);
  activityTimer = null;
  message.classList.remove("pending", "streaming");
  if (entry.revisionId) message.dataset.revisionId = entry.revisionId;
  renderMessageContent(message.querySelector(".message-body"), entry);
  renderMessageFoot(message, entry.metadata);
  messageSync.track(message, entry, { interactive: true });
}

function renderUsage() {
  tokenTotal.textContent = formatTokens(appState.usage?.totalTokens || 0).replace(
    " tok",
    "",
  );
  const limit = appState.autonomy?.dailyTokenLimit || 0;
  const today = formatTokens(appState.usage?.autonomousTokensToday || 0);
  tokenTotal.parentElement.title = limit
    ? `今日自主探索 ${today} / ${formatTokens(limit)}`
    : `今日自主探索 ${today} · 未设上限`;
}

function renderRuntimeStatus() {
  if (busy) return;
  if (appState.profile.status === "calibrating") {
    connectionStatus.textContent = "初始化对话中";
    return;
  }
  const exploration = appState.exploration;
  const phase = exploration?.phase;
  if (phase === "exploring") {
    connectionStatus.textContent =
      exploration.currentActivity?.label || "正在自主探索";
  } else if (phase === "quiet_hours") {
    connectionStatus.textContent = "在线 · 安静时段";
  } else if (phase === "token_limit") {
    connectionStatus.textContent = "在线 · 今日探索预算已用尽";
  } else if (phase === "message_limit") {
    connectionStatus.textContent = "在线 · 今日主动消息已达上限";
  } else if (phase === "error") {
    connectionStatus.textContent = "在线 · 探索运行异常";
  } else if (phase === "waiting" && exploration.nextRunAt) {
    connectionStatus.textContent = `在线 · 下次探索 ${new Date(
      exploration.nextRunAt,
    ).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
  } else {
    const reflection = appState.reflection?.runtime || appState.reflection;
    connectionStatus.textContent =
      reflection?.phase === "reflecting"
        ? reflection.currentActivity || "正在整理近期对话"
        : "在线";
  }
}

function applyRuntime(payload) {
  appState.usage = payload.usage || appState.usage;
  appState.exploration = payload.exploration || appState.exploration;
  if (payload.reflection) {
    appState.reflection = appState.reflection?.config
      ? { ...appState.reflection, runtime: payload.reflection }
      : payload.reflection;
  }
  appState.conversation = payload.conversation || appState.conversation;
  renderUsage();
  renderRuntimeStatus();
  settingsUi.renderAutonomyRuntime();
  reflectionUi.renderRuntime();
  explorationUi.runtimeUpdated();
}

async function bootstrap() {
  try {
    const response = await fetch("/api/bootstrap");
    if (!response.ok) throw new Error("无法载入当前会话。");
    const state = await response.json();
    Object.assign(appState, state);
    state.messages.forEach((message) => appendMessage(message));
    messageSync.completeBootstrap(state.messages);
    memorySize.textContent = formatMemorySize(state.memoryChars);
    renderUsage();
    renderRuntimeStatus();
    settingsUi.render();
    reflectionUi.render();
    profileUi.render();
    messageSync.start();
  } catch (error) {
    connectionStatus.textContent = "连接失败";
    appendMessage(
      {
        role: "assistant",
        at: new Date().toISOString(),
        content: error.message,
      },
      { error: true },
    );
  }
}

async function consumeStream(response, pending, outgoing) {
  if (!response.ok) {
    const payload = await response.json();
    throw new Error(payload.error || "请求失败。");
  }
  if (!response.body) throw new Error("当前浏览器无法读取流式回复。");

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let receivedText = "";
  let completed = false;

  while (true) {
    const { value, done } = await reader.read();
    buffer += decoder.decode(value || new Uint8Array(), { stream: !done });
    const lines = buffer.split("\n");
    buffer = lines.pop() || "";

    for (const line of lines) {
      if (!line.trim()) continue;
      const event = JSON.parse(line);
      if (event.type === "accepted") {
        applyAccepted(outgoing, event.message);
      } else if (event.type === "activity") {
        setActivity(pending, event);
      } else if (event.type === "delta") {
        receivedText += event.text;
        pending.classList.remove("pending");
        pending.classList.add("streaming");
        renderRichText(
          pending.querySelector(".message-body"),
          receivedText,
          { streaming: true },
        );
        conversation.scrollTop = conversation.scrollHeight;
      } else if (event.type === "reset") {
        receivedText = "";
        pending.classList.add("pending");
        pending.classList.remove("streaming");
        pending.querySelector(".message-body").textContent = "正在深入处理";
      } else if (event.type === "complete") {
        completed = true;
        for (const message of activeOutgoing) {
          messageActions.update(message, null, { deliveryState: "delivered" });
        }
        applyComplete(pending, event.message);
        memorySize.textContent = formatMemorySize(event.memoryChars);
        appState.profile = event.profile;
        appState.autonomyPermitted = event.autonomyPermitted;
        appState.usage = event.usage;
        appState.exploration = event.exploration;
        renderUsage();
        profileUi.render();
        settingsUi.renderAutonomy();
      } else if (event.type === "error") {
        throw new Error(event.error);
      }
    }
    if (done) break;
  }
  if (!completed) throw new Error("回复在完成前中断。");
}

async function sendMessage(text, images = []) {
  if (!text.trim() && !images.length) return;
  if (busy) {
    await appendToActiveResponse(text, images);
    return;
  }
  const localEntry = localUserEntry(text, images);
  const outgoing = appendMessage(localEntry, { deliveryState: "pending" });
  activeOutgoing = [outgoing];
  const pending = appendMessage(
    {
      role: "assistant",
      at: new Date().toISOString(),
      content: "等你说完",
    },
    { pending: true },
  );
  activePending = pending;
  activityStartedAt = Date.now();
  setBusy(true);

  try {
    const response = await fetch("/api/chat", {
      method: "POST",
      body: chatBody(text, images),
    });
    await consumeStream(response, pending, outgoing);
  } catch (error) {
    pending.remove();
    for (const message of activeOutgoing) {
      messageActions.update(message, null, {
        deliveryState: "failed",
        failureReason: error.message,
      });
    }
  } finally {
    activeOutgoing = [];
    activePending = null;
    signalTyping(false);
    setBusy(false);
    if (!composer.hidden) input.focus();
  }
}

async function appendToActiveResponse(text, images) {
  const outgoing = appendMessage(localUserEntry(text, images), {
    deliveryState: "pending",
  });
  if (activePending?.isConnected) {
    conversation.append(activePending);
    conversation.scrollTop = conversation.scrollHeight;
  }
  activeOutgoing.push(outgoing);
  signalTyping(false);
  try {
    const response = await fetch("/api/chat/append", {
      method: "POST",
      body: chatBody(text, images),
    });
    const entry = await response.json();
    if (!response.ok) throw new Error(entry.error || "无法追加消息。");
    applyAccepted(outgoing, entry);
    composerState.textContent = "已接入当前思考";
    window.setTimeout(() => {
      if (composerState.textContent === "已接入当前思考") {
        composerState.textContent = "";
      }
    }, 1200);
  } catch (error) {
    messageActions.update(outgoing, null, {
      deliveryState: "failed",
      failureReason: error.message,
    });
  }
}

function localUserEntry(text, images) {
  return {
    role: "user",
    at: new Date().toISOString(),
    content: text,
    parts: [
      ...(text.trim() ? [{ type: "markdown", text: text.trim() }] : []),
      ...images.map((image) => ({
        type: "image",
        asset: {
          assetId: image.file.name,
          url: image.url,
          filename: image.file.name,
          mimeType: image.file.type,
          byteSize: image.file.size,
          file: image.file,
        },
      })),
    ],
    deliveryState: "pending",
  };
}

function chatBody(text, images) {
  const body = new FormData();
  body.append("message", text);
  for (const image of images) body.append("image", image.file, image.file.name);
  return body;
}

function signalTyping(typing) {
  clearTimeout(typingSignalTimer);
  if (!busy) return;
  fetch("/api/interaction/typing", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ typing }),
    keepalive: true,
  }).catch(() => {});
  if (typing) {
    typingSignalTimer = setTimeout(() => signalTyping(false), 2200);
  }
}

async function performMessageAction(action, message, entry) {
  if (action === "recall" || action === "delete") {
    await retractMessage(message, entry);
    return;
  }
  const images = await recoverImages(entry);
  if (action === "edit") {
    await retractMessage(message, entry);
    input.value = entry.content || "";
    selectedImages = images;
    renderAttachmentTray();
    resizeComposer();
    input.focus();
    return;
  }
  if (action === "retry") {
    await retractMessage(message, entry);
    await sendMessage(entry.content || "", images);
  }
}

async function retractMessage(message, entry) {
  if (!entry.revisionId) {
    removeMessages([], message);
    return;
  }
  const response = await fetch(
    `/api/messages/${encodeURIComponent(entry.revisionId)}`,
    { method: "DELETE" },
  );
  const payload = await response.json();
  if (!response.ok) throw new Error(payload.error || "无法撤回消息");
  removeMessages(payload.removedRevisionIds || [], message);
  memorySize.textContent = formatMemorySize(payload.memoryChars || 0);
}

function removeMessages(revisionIds, fallback) {
  messageSync.remove(revisionIds);
  for (const revisionId of revisionIds) {
    conversation
      .querySelector(
        `.message[data-revision-id="${CSS.escape(revisionId)}"]`,
      )
      ?.remove();
  }
  if (fallback?.isConnected) fallback.remove();
  emptyState.hidden = Boolean(conversation.querySelector(".message"));
  messageActions.refresh();
}

async function recoverImages(entry) {
  const images = [];
  for (const part of entry.parts || []) {
    if (part.type !== "image" || !part.asset) continue;
    const asset = part.asset;
    if (asset.file instanceof File) {
      images.push({ file: asset.file, url: asset.url });
      continue;
    }
    const response = await fetch(asset.url);
    if (!response.ok) throw new Error(`无法重新读取图片 ${asset.filename}`);
    const blob = await response.blob();
    const file = new File([blob], asset.filename || "image", {
      type: asset.mimeType || blob.type,
    });
    images.push({ file, url: URL.createObjectURL(file) });
  }
  return images;
}

composer.addEventListener("submit", (event) => {
  event.preventDefault();
  const text = input.value.trim();
  const images = selectedImages;
  if (!text && !images.length) return;
  input.value = "";
  selectedImages = [];
  composerState.textContent = "";
  renderAttachmentTray();
  resizeComposer();
  signalTyping(false);
  sendMessage(text, images);
});

input.addEventListener("input", () => {
  resizeComposer();
  if (busy) signalTyping(Boolean(input.value.trim()));
});
input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    composer.requestSubmit();
  }
});

addImageButton.addEventListener("click", () => imageInput.click());
imageInput.addEventListener("change", () => {
  addImages([...imageInput.files]);
  imageInput.value = "";
});
input.addEventListener("paste", (event) => {
  const images = [...event.clipboardData.items]
    .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
    .map((item) => item.getAsFile())
    .filter(Boolean);
  if (images.length) {
    event.preventDefault();
    addImages(images);
  }
});
composer.addEventListener("dragover", (event) => {
  if ([...event.dataTransfer.types].includes("Files")) {
    event.preventDefault();
    composer.classList.add("dragging");
  }
});
composer.addEventListener("dragleave", () => composer.classList.remove("dragging"));
composer.addEventListener("drop", (event) => {
  composer.classList.remove("dragging");
  const images = [...event.dataTransfer.files].filter((file) =>
    file.type.startsWith("image/"),
  );
  if (images.length) {
    event.preventDefault();
    addImages(images);
  }
});

function addImages(files) {
  composerState.textContent = "";
  for (const file of files) {
    if (selectedImages.length >= MAX_IMAGES) {
      composerState.textContent = `每条消息最多 ${MAX_IMAGES} 张图片`;
      break;
    }
    if (!["image/jpeg", "image/png", "image/webp", "image/gif"].includes(file.type)) {
      composerState.textContent = "支持 JPEG、PNG、WebP 和 GIF";
      continue;
    }
    if (file.size > MAX_IMAGE_BYTES) {
      composerState.textContent = `${file.name} 超过 15 MB`;
      continue;
    }
    selectedImages.push({ file, url: URL.createObjectURL(file) });
  }
  renderAttachmentTray();
}

function renderAttachmentTray() {
  attachmentTray.replaceChildren();
  attachmentTray.hidden = selectedImages.length === 0;
  selectedImages.forEach((image, index) => {
    const preview = document.createElement("figure");
    preview.className = "attachment-preview";
    const thumbnail = document.createElement("img");
    thumbnail.src = image.url;
    thumbnail.alt = image.file.name;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.title = `移除 ${image.file.name}`;
    remove.setAttribute("aria-label", `移除 ${image.file.name}`);
    remove.textContent = "×";
    remove.addEventListener("click", () => {
      const [removed] = selectedImages.splice(index, 1);
      URL.revokeObjectURL(removed.url);
      renderAttachmentTray();
    });
    preview.append(thumbnail, remove);
    attachmentTray.append(preview);
  });
}

document
  .querySelector("#open-usage")
  .addEventListener("click", () => settingsUi.open("stats"));

for (const button of document.querySelectorAll("[data-close]")) {
  button.addEventListener("click", () => {
    document.querySelector(`#${button.dataset.close}`).close();
  });
}
for (const dialog of document.querySelectorAll("dialog")) {
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });
}

bootstrap();
resizeComposer();
