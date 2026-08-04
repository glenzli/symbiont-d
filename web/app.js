import { initProfileUi } from "/profile-ui.js";
import { initReflectionUi } from "/reflection-ui.js";
import { initReconciliationUi } from "/reconciliation-ui.js";
import { formatDuration, formatMemorySize, formatTokens } from "/presentation.js";
import { renderMessageContent, renderRichText } from "/rich-text.js";
import { initExplorationUi } from "/exploration-ui.js";
import { initIdentityUi } from "/identity-ui.js";
import { initComputeModeUi } from "/compute-mode-ui.js";
import { initComposerContextUi } from "/composer-context-ui.js";
import { initCodexContextUi } from "/codex-context-ui.js";
import { initMessageActions } from "/message-actions.js";
import { initMessageSync } from "/message-sync.js";
import { initPermissionUi } from "/permission-ui.js";
import { initQuoteUi } from "/quote-ui.js";
import { initSettings } from "/settings.js";
import { initTopbarUi } from "/topbar-ui.js";
import { initTopicUi } from "/topic-ui.js";
import { initTraceUi } from "/trace-ui.js";
import { initTurnDispositionUi } from "/turn-disposition-ui.js";
import { renderIcons } from "/icons.js";

const appState = {
  models: [],
  compute: null,
  computePolicies: [],
  identity: { avatar: null },
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
  reconciliation: null,
  memoryIndex: null,
  conversation: null,
  bridge: {
    codexTaskAccess: false,
  },
  permissions: [],
};

const conversation = document.querySelector("#conversation");
const emptyState = document.querySelector("#empty-state");
const composer = document.querySelector("#composer");
const input = document.querySelector("#message");
const computeMode = document.querySelector("#compute-mode");
const sendButton = document.querySelector("#send");
const stopResponseButton = document.querySelector("#stop-response");
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
let composerNoticeTimer = null;
let stoppingResponse = false;
const manualExplorationReceiptIds = new Set();

const MAX_IMAGES = 4;
const MAX_IMAGE_BYTES = 15 * 1024 * 1024;

const explorationUi = initExplorationUi(appState, {
  announceManualSilence: appendManualExplorationReceipt,
});
const settingsUi = initSettings(appState, explorationUi.trigger);
const identityUi = initIdentityUi(appState);
const permissionUi = initPermissionUi(appState);
const composerContextUi = initComposerContextUi({
  state: appState,
  chooseImage: () => imageInput.click(),
  notify: notifyComposer,
  openSettings: settingsUi.open,
});
initCodexContextUi(() => input.value, notifyComposer);
const reflectionUi = initReflectionUi(appState);
const reconciliationUi = initReconciliationUi(appState);
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
const turnDispositionUi = initTurnDispositionUi({
  conversation,
  messageActions,
  notify: notifyComposer,
});
const quoteUi = initQuoteUi({
  conversation,
  entryFor: messageActions.entryFor,
  focusComposer() {
    input.focus();
    resizeComposer();
  },
  notify: notifyComposer,
});
const topicUi = initTopicUi({
  conversation,
  focusComposer() {
    input.focus();
    resizeComposer();
  },
});
const computeModeUi = initComputeModeUi();
initTopbarUi();
renderIcons();

function metadataText(metadata) {
  if (!metadata?.runs?.length || appState.compute?.showModel === false) return "";
  const runs = metadata.runs.map((run) => {
    const name = run.displayName || run.model;
    const lane = {
      sense: "感知",
      observe: "观察",
      conversation: "日常",
      investigate: "深入",
      critical: "关键",
    }[run.lane];
    return [name, run.effort, lane].filter(Boolean).join(" · ");
  });
  const origin = {
    autonomous: "主动探索",
    continuation: "续话",
  }[metadata.origin] || "";
  return [
    origin,
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
  const avatar = fragment.querySelector(".message-avatar");
  const role = entry.role || "assistant";

  article.dataset.role = role;
  if (entry.revisionId) article.dataset.revisionId = entry.revisionId;
  if (options.pending) article.classList.add("pending");
  if (options.error) article.classList.add("error");
  speaker.textContent = role === "user" ? "你" : "symbiont-d";
  identityUi.applyAvatar(avatar, role === "user" ? "user" : "symbiont");
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

function appendManualExplorationReceipt(receipt) {
  if (!receipt?.id || manualExplorationReceiptIds.has(receipt.id)) return;
  manualExplorationReceiptIds.add(receipt.id);
  emptyState.hidden = true;

  const notice = document.createElement("article");
  notice.className = "conversation-notice exploration-receipt";
  notice.dataset.receiptId = receipt.id;
  notice.setAttribute("role", "status");

  const label = document.createElement("strong");
  label.textContent = "探索完成";
  const message = document.createElement("span");
  message.textContent = "本次没有发现值得打扰你的新情报。";
  const time = document.createElement("time");
  time.dateTime = receipt.completedAt;
  time.textContent = new Date(receipt.completedAt).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  notice.append(label, message, time);
  conversation.append(notice);
  conversation.scrollTop = conversation.scrollHeight;
}

function resizeComposer() {
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 140)}px`;
}

function setBusy(nextBusy) {
  busy = nextBusy;
  composer.classList.toggle("response-active", nextBusy);
  sendButton.hidden = nextBusy;
  stopResponseButton.hidden = !nextBusy;
  messageActions.refresh();
  if (!nextBusy) {
    stoppingResponse = false;
    stopResponseButton.disabled = false;
    renderRuntimeStatus();
    clearInterval(activityTimer);
    activityTimer = null;
  }
}

function setActivity(message, event) {
  const body = message.querySelector(".message-body");
  const foot = message.querySelector(".message-foot");
  const runtime = foot.querySelector(".message-runtime");
  message.classList.remove("response-placeholder");
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
  message.classList.remove("pending", "streaming", "response-placeholder");
  if (entry.revisionId) message.dataset.revisionId = entry.revisionId;
  renderMessageContent(message.querySelector(".message-body"), entry);
  renderMessageFoot(message, entry.metadata);
  messageSync.track(message, entry, { interactive: true });
  messageActions.update(message, entry);
}

function applyCompletionProjection(event) {
  memorySize.textContent = formatMemorySize(event.memoryChars);
  appState.profile = event.profile;
  appState.autonomyPermitted = event.autonomyPermitted;
  appState.usage = event.usage;
  appState.exploration = event.exploration;
  appState.computePolicies = event.computePolicies || appState.computePolicies;
  renderUsage();
  profileUi.render();
  settingsUi.renderAutonomy();
  explorationUi.runtimeUpdated();
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
    connectionStatus.textContent = "在线 · 今日主动消息额度已用尽";
  } else if (phase === "error") {
    connectionStatus.textContent = "在线 · 探索运行异常";
  } else if (phase === "waiting" && exploration.nextRunAt) {
    const candidates = exploration.pendingCandidateCount
      ? ` · ${exploration.pendingCandidateCount} 条候选待复核`
      : "";
    connectionStatus.textContent = `在线 · 下次感知 ${new Date(
      exploration.nextRunAt,
    ).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}${candidates}`;
  } else {
    const reflection = appState.reflection?.runtime || appState.reflection;
    connectionStatus.textContent =
      reflection?.phase === "reflecting"
        ? reflection.currentActivity || "正在整理近期对话"
        : "在线";
  }
}

function applyRuntime(payload) {
  appState.identity = payload.identity || appState.identity;
  appState.usage = payload.usage || appState.usage;
  appState.exploration = payload.exploration || appState.exploration;
  if (payload.reflection) {
    appState.reflection = appState.reflection?.config
      ? { ...appState.reflection, runtime: payload.reflection }
      : payload.reflection;
  }
  if (payload.reconciliation) {
    appState.reconciliation = appState.reconciliation?.recentRuns
      ? { ...appState.reconciliation, runtime: payload.reconciliation }
      : payload.reconciliation;
  }
  appState.memoryIndex = payload.memoryIndex || appState.memoryIndex;
  appState.conversation = payload.conversation || appState.conversation;
  appState.computePolicies =
    payload.computePolicies || appState.computePolicies;
  appState.permissions = payload.permissions || appState.permissions;
  appState.bridge = payload.bridge || appState.bridge;
  renderUsage();
  renderRuntimeStatus();
  identityUi.render();
  settingsUi.renderAutonomyRuntime();
  reflectionUi.renderRuntime();
  reconciliationUi.runtimeUpdated();
  explorationUi.runtimeUpdated();
  composerContextUi.configUpdated();
  permissionUi.render();
  turnDispositionUi.applyAll(payload.turnDispositions);
}

async function bootstrap() {
  try {
    const response = await fetch("/api/bootstrap");
    if (!response.ok) throw new Error("无法载入当前会话。");
    const state = await response.json();
    Object.assign(appState, state);
    state.messages.forEach((message) => appendMessage(message));
    turnDispositionUi.applyAll(state.turnDispositions);
    messageSync.completeBootstrap(state.messages);
    memorySize.textContent = formatMemorySize(state.memoryChars);
    renderUsage();
    renderRuntimeStatus();
    identityUi.render();
    settingsUi.render();
    explorationUi.runtimeUpdated();
    composerContextUi.configUpdated();
    composerContextUi.warm();
    reflectionUi.render();
    reconciliationUi.render();
    profileUi.render();
    permissionUi.render();
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
  let interrupted = false;
  let settled = null;

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
        pending.classList.remove("pending", "response-placeholder");
        pending.classList.add("streaming");
        renderRichText(
          pending.querySelector(".message-body"),
          receivedText,
          { streaming: true },
        );
        conversation.scrollTop = conversation.scrollHeight;
      } else if (event.type === "reset") {
        receivedText = "";
        pending.classList.add("pending", "response-placeholder");
        pending.classList.remove("streaming");
        pending.querySelector(".message-body").textContent = "";
        connectionStatus.textContent = "正在回应";
      } else if (event.type === "interrupted") {
        interrupted = true;
      } else if (event.type === "settled") {
        settled = event;
        applyCompletionProjection(event);
      } else if (event.type === "complete") {
        completed = true;
        for (const message of activeOutgoing) {
          messageActions.update(message, null, { deliveryState: "delivered" });
        }
        applyComplete(pending, event.message);
        applyCompletionProjection(event);
      } else if (event.type === "error") {
        throw new Error(event.error);
      }
    }
    if (done) break;
  }
  if (!completed && !interrupted && !settled) {
    throw new Error("回复在完成前中断。");
  }
  return { interrupted, settled };
}

async function sendMessage(
  text,
  images = [],
  minimumLane = "auto",
  quotes = [],
  topic = null,
  codexTaskIds = [],
) {
  if (!text.trim() && !images.length && !quotes.length && !codexTaskIds.length) return;
  if (busy) {
    await appendToActiveResponse(
      text,
      images,
      minimumLane,
      quotes,
      topic,
      codexTaskIds,
    );
    return;
  }
  const localEntry = localUserEntry(text, images, quotes, topic);
  const outgoing = appendMessage(localEntry, { deliveryState: "pending" });
  activeOutgoing = [outgoing];
  const pending = appendMessage(
    {
      role: "assistant",
      at: new Date().toISOString(),
      content: "",
    },
    { pending: true },
  );
  pending.classList.add("response-placeholder");
  pending
    .querySelector(".message-body")
    .setAttribute("aria-label", "symbiont-d 正在回应");
  activePending = pending;
  activityStartedAt = Date.now();
  setBusy(true);
  connectionStatus.textContent = "正在回应";

  try {
    const response = await fetch("/api/chat", {
      method: "POST",
      body: chatBody(text, images, minimumLane, quotes, topic, codexTaskIds),
    });
    const result = await consumeStream(response, pending, outgoing);
    if (result.interrupted) {
      pending.remove();
      for (const message of activeOutgoing) {
        messageActions.update(message, null, { deliveryState: "stopped" });
      }
      notifyComposer("已停止回复");
    } else if (result.settled) {
      pending.remove();
      for (const message of activeOutgoing) {
        messageActions.update(message, null, { deliveryState: "delivered" });
      }
      turnDispositionUi.apply(result.settled, { announce: true });
    }
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

async function appendToActiveResponse(
  text,
  images,
  minimumLane,
  quotes,
  topic,
  codexTaskIds = [],
) {
  const outgoing = appendMessage(localUserEntry(text, images, quotes, topic), {
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
      body: chatBody(text, images, minimumLane, quotes, topic, codexTaskIds),
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

function localUserEntry(text, images, quotes, topic) {
  return {
    role: "user",
    at: new Date().toISOString(),
    content: text,
    parts: [
      ...(topic
        ? [
            {
              type: "topic",
              topic: { topicId: topic.id, title: topic.title },
            },
          ]
        : []),
      ...quotes.map((quote) => ({
        type: "quote",
        quote: {
          sourceRevisionId: quote.sourceRevisionId,
          sourceRole: quote.sourceRole || "assistant",
          sourceAt: quote.sourceAt || new Date().toISOString(),
          text: quote.text || quote.selectedText,
          sourceSha256: quote.sourceSha256 || "",
          startOffset: quote.startOffset ?? null,
          endOffset: quote.endOffset ?? null,
          wholeMessage: quote.wholeMessage === true,
          truncated: quote.truncated === true,
        },
      })),
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

function chatBody(
  text,
  images,
  minimumLane = "auto",
  quotes = [],
  topic = null,
  codexTaskIds = [],
) {
  const body = new FormData();
  body.append("message", text);
  body.append("computeLane", minimumLane);
  if (topic?.id) body.append("topicId", topic.id);
  for (const quote of quotes) body.append("quote", JSON.stringify(quote));
  for (const image of images) body.append("image", image.file, image.file.name);
  for (const taskId of codexTaskIds) body.append("codexTaskId", taskId);
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

function notifyComposer(message) {
  clearTimeout(composerNoticeTimer);
  composerState.textContent = message;
  composerNoticeTimer = window.setTimeout(() => {
    if (composerState.textContent === message) composerState.textContent = "";
  }, 2200);
}

async function performMessageAction(action, message, entry) {
  if (action === "quote") {
    quoteUi.addWhole(entry);
    return;
  }
  if (action === "copy") {
    const text = String(entry.content || "").trim();
    if (!text) return;
    try {
      await copyMessageText(text);
      notifyComposer("已复制");
    } catch (error) {
      notifyComposer(error.message);
    }
    return;
  }
  if (action === "recall" || action === "delete") {
    await retractMessage(message, entry);
    return;
  }
  const images = await recoverImages(entry);
  if (action === "edit") {
    await retractMessage(message, entry);
    input.value = entry.content || "";
    selectedImages = images;
    quoteUi.set(extractQuotes(entry));
    const topic = extractTopic(entry);
    if (topic) topicUi.set(topic);
    else topicUi.clear();
    renderAttachmentTray();
    resizeComposer();
    input.focus();
    return;
  }
  if (action === "retry") {
    await retractMessage(message, entry);
    await sendMessage(
      entry.content || "",
      images,
      "auto",
      extractQuotes(entry),
      extractTopic(entry),
    );
  }
}

async function copyMessageText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const fallback = document.createElement("textarea");
    fallback.value = text;
    fallback.setAttribute("readonly", "");
    fallback.style.position = "fixed";
    fallback.style.opacity = "0";
    document.body.append(fallback);
    fallback.select();
    const copied = document.execCommand("copy");
    fallback.remove();
    if (!copied) throw new Error("无法复制消息文本");
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

function extractQuotes(entry) {
  return (entry.parts || [])
    .filter((part) => part.type === "quote" && part.quote)
    .map((part) => part.quote);
}

function extractTopic(entry) {
  const reference = (entry.parts || []).find(
    (part) => part.type === "topic" && part.topic?.topicId,
  )?.topic;
  return reference
    ? { id: reference.topicId, title: reference.title || "未命名主题" }
    : null;
}

composer.addEventListener("submit", (event) => {
  event.preventDefault();
  if (stoppingResponse) {
    notifyComposer("正在停止上一条回复");
    return;
  }
  const text = input.value.trim();
  const images = selectedImages;
  const quotes = quoteUi.drafts();
  const codexTaskIds = composerContextUi.consume();
  const minimumLane = computeMode.value;
  if (codexTaskIds === null) return;
  if (!text && !images.length && !quotes.length && !codexTaskIds.length) return;
  const topic = topicUi.consume();
  input.value = "";
  selectedImages = [];
  quoteUi.clear();
  computeModeUi.reset();
  composerState.textContent = "";
  renderAttachmentTray();
  resizeComposer();
  signalTyping(false);
  sendMessage(text, images, minimumLane, quotes, topic, codexTaskIds);
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

stopResponseButton.addEventListener("click", () => {
  stopActiveResponse();
});
imageInput.addEventListener("change", () => {
  addImages([...imageInput.files]);
  imageInput.value = "";
});

async function stopActiveResponse() {
  if (!busy || stoppingResponse) return;
  stoppingResponse = true;
  stopResponseButton.disabled = true;
  composerState.textContent = "正在停止回复";
  try {
    const response = await fetch("/api/chat/interrupt", { method: "POST" });
    const payload = await response.json();
    if (!response.ok) throw new Error(payload.error || "无法停止回复");
    if (!payload.accepted) {
      notifyComposer("回复已结束");
    }
  } catch (error) {
    stoppingResponse = false;
    stopResponseButton.disabled = false;
    composerState.textContent = error.message;
  }
}
input.addEventListener("paste", (event) => {
  const clipboardData = event.clipboardData;
  const images = clipboardImages(clipboardData);
  if (images.length) {
    event.preventDefault();
    addPastedImages(images, false).catch((error) => {
      composerState.textContent = error.message;
    });
    return;
  }

  if (!clipboardMayContainImageWithoutText(clipboardData)) return;
  event.preventDefault();
  addPastedImages([], true).catch((error) => {
    composerState.textContent = error.message;
  });
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

function clipboardImages(clipboardData) {
  if (!clipboardData) return [];
  const candidates = [
    ...Array.from(clipboardData.files || []),
    ...Array.from(clipboardData.items || [])
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter(Boolean),
  ].filter((file) => file.type.startsWith("image/"));
  const seen = new Set();
  return candidates.filter((file) => {
    const key = [file.name, file.type, file.size, file.lastModified].join(":");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function clipboardMayContainImageWithoutText(clipboardData) {
  if (!clipboardData || clipboardContainsText(clipboardData)) return false;
  return Array.from(clipboardData.types || []).includes("Files");
}

function clipboardContainsText(clipboardData) {
  const types = Array.from(clipboardData.types || []);
  if (types.some((type) => type.startsWith("text/"))) return true;
  return Boolean(
    clipboardData.getData?.("text/plain") || clipboardData.getData?.("text/html"),
  );
}

async function addPastedImages(images, mayContainFiles) {
  let files = images;
  if (!files.length && mayContainFiles && navigator.clipboard?.read) {
    try {
      files = await readClipboardImages();
    } catch {
      throw new Error("无法读取剪贴板中的图片");
    }
  }
  if (!files.length) throw new Error("无法读取剪贴板中的图片");
  composerState.textContent = "正在读取剪贴板图片";
  const normalized = await Promise.all(files.map(normalizePastedImage));
  composerState.textContent = "";
  addImages(normalized);
}

async function readClipboardImages() {
  const files = [];
  for (const item of await navigator.clipboard.read()) {
    for (const type of item.types.filter((type) => type.startsWith("image/"))) {
      const blob = await item.getType(type);
      files.push(new File([blob], clipboardFilename(type, files.length), { type }));
    }
  }
  return files;
}

async function normalizePastedImage(file, index) {
  const supported = ["image/jpeg", "image/png", "image/webp", "image/gif"];
  if (supported.includes(file.type)) {
    return file.name
      ? file
      : new File([file], clipboardFilename(file.type, index), { type: file.type });
  }
  if (!file.type.startsWith("image/")) return file;
  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = image.naturalWidth;
    canvas.height = image.naturalHeight;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("无法转换剪贴板图片");
    context.drawImage(image, 0, 0);
    const blob = await new Promise((resolve, reject) => {
      canvas.toBlob(
        (value) => value ? resolve(value) : reject(new Error("无法转换剪贴板图片")),
        "image/png",
      );
    });
    return new File([blob], clipboardFilename("image/png", index), {
      type: "image/png",
    });
  } finally {
    URL.revokeObjectURL(url);
  }
}

function clipboardFilename(type, index) {
  const extension = {
    "image/jpeg": "jpg",
    "image/png": "png",
    "image/webp": "webp",
    "image/gif": "gif",
  }[type] || "png";
  return `clipboard-${Date.now()}-${index + 1}.${extension}`;
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
