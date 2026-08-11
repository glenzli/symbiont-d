import { initProfileUi } from "/profile-ui.js";
import { initReflectionUi } from "/reflection-ui.js";
import { initReconciliationUi } from "/reconciliation-ui.js";
import { formatDuration, formatMemorySize, formatTokens } from "/presentation.js";
import { renderMessageContent, renderRichText } from "/rich-text.js";
import { initExplorationUi } from "/exploration-ui.js";
import { manualCompletionNotice } from "/exploration-receipt.js";
import { initIdentityUi } from "/identity-ui.js";
import { applyInputRoleAvatar, initInputRoleUi } from "/input-roles.js";
import { regroupInputSignals } from "/input-signal-groups.js";
import { initConversationFocusUi } from "/conversation-focus-ui.js";
import { initComputeModeUi } from "/compute-mode-ui.js";
import { initComposerContextUi } from "/composer-context-ui.js";
import { initVoiceInput } from "/voice-input.js";
import { initMessageActions } from "/message-actions.js";
import { initMessageSync } from "/message-sync.js";
import { initPermissionUi } from "/permission-ui.js";
import { initQuoteUi, quoteDraft } from "/quote-ui.js";
import { initSettings } from "/settings.js";
import { initUsageUi } from "/usage-ui.js";
import { initTopbarUi } from "/topbar-ui.js";
import { initTopicUi } from "/topic-ui.js";
import { initTraceUi } from "/trace-ui.js";
import { initTurnDispositionUi } from "/turn-disposition-ui.js";
import { initEphemeralDiscussionUi } from "/ephemeral-discussion-ui.js";
import { renderIcons } from "/icons.js";

const appState = {
  models: [],
  compute: null,
  ambient: null,
  driveInput: null,
  mailInput: null,
  inputRoles: { roles: [], avatarOptions: [] },
  audioTranscription: null,
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
  attacker: null,
  reflection: null,
  reconciliation: null,
  memoryIndex: null,
  conversation: null,
  bridge: {
    codexTaskAccess: false,
  },
  signals: [],
  permissions: [],
};

const conversation = document.querySelector("#conversation");
const emptyState = document.querySelector("#empty-state");
const temporaryDiscussionConversation = document.querySelector(
  "#temporary-discussion-conversation",
);
const temporaryDiscussionEmpty = document.querySelector(
  "#temporary-discussion-empty",
);
const composer = document.querySelector("#composer");
const input = document.querySelector("#message");
const computeMode = document.querySelector("#compute-mode");
const sendButton = document.querySelector("#send");
const voiceInputButton = document.querySelector("#voice-input");
const voiceRecordingStatus = document.querySelector("#voice-recording-status");
const voiceRecordingLabel = document.querySelector("#voice-recording-label");
const voiceWaveform = document.querySelector("#voice-waveform");
const voiceRecordingElapsed = document.querySelector("#voice-recording-elapsed");
const stopResponseButton = document.querySelector("#stop-response");
const imageInput = document.querySelector("#image-input");
const attachmentTray = document.querySelector("#attachment-tray");
const appStatusBar = document.querySelector("#app-statusbar");
const composerState = document.querySelector("#app-status-message");
const appStatusRuntime = document.querySelector("#app-status-runtime");
const connectionStatus = document.querySelector("#connection-status");
const memorySize = document.querySelector("#memory-size");
const tokenTotal = document.querySelector("#token-total");
const messageTemplate = document.querySelector("#message-template");
const scrollToLatestButton = document.querySelector("#scroll-to-latest");
const conversationFocusButton = document.querySelector("#toggle-conversation-focus");
const conversationFocusLabel = document.querySelector("#conversation-focus-label");
const conversationFocusBanner = document.querySelector("#conversation-focus-banner");
const conversationFocusHiddenCount = document.querySelector(
  "#conversation-focus-hidden-count",
);
const conversationFocusShowAll = document.querySelector("#conversation-focus-show-all");

let busy = false;
let activityStartedAt = 0;
let activityTimer = null;
let selectedImages = [];
let activeOutgoing = [];
let activePending = null;
let typingSignalTimer = null;
let composerNoticeTimer = null;
let responseWaitTimer = null;
let responseDelayTimer = null;
let stoppingResponse = false;
let selectedSignalId = null;
const manualExplorationReceiptIds = new Set();
const displayedSignalIds = new Set();

const MAX_IMAGES = 4;
const MAX_IMAGE_BYTES = 15 * 1024 * 1024;
const SCROLL_TO_LATEST_THRESHOLD = 120;

const explorationUi = initExplorationUi(appState, {
  announceManualCompletion: appendManualExplorationReceipt,
});
const usageUi = initUsageUi(appState);
const identityUi = initIdentityUi(appState);
const inputRoleUi = initInputRoleUi(appState);
const settingsUi = initSettings(appState, {
  saveInputRoles: inputRoleUi.save,
  refreshInputRoles: inputRoleUi.refresh,
});
const permissionUi = initPermissionUi(appState);
const composerContextUi = initComposerContextUi({
  state: appState,
  chooseImage: () => imageInput.click(),
  notify: notifyComposer,
  openSettings: settingsUi.open,
});
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
  appendEphemeralReaction,
  notify: notifyComposer,
});
initConversationFocusUi({
  conversation,
  button: conversationFocusButton,
  buttonLabel: conversationFocusLabel,
  banner: conversationFocusBanner,
  hiddenCount: conversationFocusHiddenCount,
  showAllButton: conversationFocusShowAll,
  renderIcons,
  notify: notifyComposer,
});

function updateScrollToLatestControl() {
  const distanceFromLatest =
    conversation.scrollHeight - conversation.scrollTop - conversation.clientHeight;
  scrollToLatestButton.hidden = distanceFromLatest <= SCROLL_TO_LATEST_THRESHOLD;
}

conversation.addEventListener("scroll", updateScrollToLatestControl, {
  passive: true,
});
scrollToLatestButton.addEventListener("click", () => {
  conversation.scrollTo({ top: conversation.scrollHeight, behavior: "smooth" });
  requestAnimationFrame(updateScrollToLatestControl);
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
  applyAvatar: identityUi.applyAvatar,
  focusComposer() {
    input.focus();
    resizeComposer();
  },
});
const computeModeUi = initComputeModeUi();
const voiceInput = initVoiceInput({
  state: appState,
  input,
  button: voiceInputButton,
  status: voiceRecordingStatus,
  statusLabel: voiceRecordingLabel,
  waveform: voiceWaveform,
  elapsed: voiceRecordingElapsed,
  notify: notifyComposer,
  setPersistentStatus,
  resize: resizeComposer,
});
const ephemeralDiscussionUi = initEphemeralDiscussionUi({
  notify: notifyComposer,
  renderSnapshot: renderTemporaryDiscussionSnapshot,
  renderPending: renderTemporaryDiscussionPending,
  clearMessages: clearTemporaryDiscussionMessages,
  appendPromoted(entry) {
    appendMessage(entry);
  },
  canActivate: () => !busy,
  onBusyChange(nextBusy) {
    setBusy(nextBusy);
    connectionStatus.textContent = nextBusy ? "独立讨论中" : "在线";
    if (nextBusy) setRuntimeStatus("独立讨论正在回应", "working");
  },
});
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
  if (!foot) return;
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
  const target = options.container || conversation;
  if (target === conversation) emptyState.hidden = true;
  if (target === temporaryDiscussionConversation) {
    temporaryDiscussionEmpty.hidden = true;
  }
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
  if (options.temporary) article.classList.add("temporary-discussion-message");
  speaker.textContent = role === "user" ? "你" : "symbiont-d";
  identityUi.applyAvatar(avatar, role === "user" ? "user" : "symbiont");
  time.dateTime = entry.at || new Date().toISOString();
  time.textContent = new Date(time.dateTime).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  renderMessageContent(body, entry);
  renderMessageFoot(article, entry.metadata);
  if (options.temporary) {
    const foot = article.querySelector(".message-foot");
    foot.querySelector(".message-runtime").textContent = "临时";
    foot.hidden = false;
  }
  target.append(fragment);
  const element = target.lastElementChild;
  if (target === conversation && !options.temporary) {
    messageSync.track(element, entry, options);
    messageActions.track(element, entry, {
      deliveryState: options.deliveryState,
      failureReason: options.failureReason,
    });
  }
  if (options.scroll !== false) {
    target.scrollTop = target.scrollHeight;
  }
  return element;
}

function appendEphemeralReaction({ revisionId, reaction }) {
  const source = conversation.querySelector(
    `.message[data-role="user"][data-revision-id="${CSS.escape(revisionId)}"]`,
  );
  if (!source || !reaction) return null;
  const selector = `.message.ephemeral-reaction-message[data-reacts-to="${CSS.escape(revisionId)}"]`;
  const existing = conversation.querySelector(selector);
  if (existing) return existing;

  const sourceTime = source.querySelector("time")?.dateTime || new Date().toISOString();
  const message = appendMessage(
    {
      role: "assistant",
      at: sourceTime,
      content: reaction,
      parts: [{ type: "markdown", text: reaction }],
    },
    { scroll: false },
  );
  message.classList.add("ephemeral-reaction-message");
  message.dataset.reactsTo = revisionId;
  source.after(message);
  return message;
}

function appendInputSignal(signal, options = {}) {
  if (!signal?.id || displayedSignalIds.has(signal.id)) return null;
  displayedSignalIds.add(signal.id);
  emptyState.hidden = true;

  const article = document.createElement("article");
  article.className = "message input-signal";
  article.dataset.signalId = signal.id;
  article.dataset.inputRoleId = signal.actor?.id || "";
  article.dataset.signalKind = signal.kind || "external_input";
  if (signal.kind === "attacker_challenge") article.classList.add("attacker-challenge");
  article.dataset.signalObservedAt =
    signal.observedAt || signal.observed_at || new Date().toISOString();
  const layout = document.createElement("div");
  layout.className = "message-layout";
  const avatar = document.createElement("div");
  avatar.className = "message-avatar input-role-avatar";
  avatar.setAttribute("aria-hidden", "true");
  applyInputRoleAvatar(avatar, signal.actor?.avatarSeed || signal.actor?.avatar_seed);
  const content = document.createElement("div");
  content.className = "message-content";
  const meta = document.createElement("div");
  meta.className = "message-meta";
  const speaker = document.createElement("span");
  speaker.className = "speaker";
  speaker.textContent = signal.actor?.name || "广域输入";
  const time = document.createElement("time");
  time.dateTime = signal.observedAt || signal.observed_at || new Date().toISOString();
  time.textContent = new Date(time.dateTime).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  const label = document.createElement("span");
  label.className = "input-signal-label";
  const observedAt = new Date(signal.observedAt || signal.observed_at || Date.now());
  const rawEventAt = signal.eventAt || signal.event_at;
  const eventAt = rawEventAt ? new Date(rawEventAt) : null;
  const eventLabel = eventAt && !Number.isNaN(eventAt.getTime())
    ? `发生于 ${eventAt.toLocaleDateString([], { month: "numeric", day: "numeric" })}`
    : null;
  const observedLabel = !Number.isNaN(observedAt.getTime())
    ? `采集于 ${observedAt.toLocaleDateString([], { month: "numeric", day: "numeric" })}`
    : null;
  label.textContent = [signal.kind === "attacker_challenge" ? "逆向审视" : "外部输入", eventLabel, observedLabel].filter(Boolean).join(" · ");
  meta.append(speaker, label, time);
  const receivedText = signal.receivedText || signal.received_text || signal.summary || "";
  const displayText = signal.content || receivedText || signal.title;
  const body = document.createElement("div");
  body.className = "message-body";
  renderMessageContent(body, {
    content: displayText,
    parts: [{ type: "markdown", text: displayText }],
  });
  const foot = document.createElement("div");
  foot.className = "message-foot input-signal-foot";
  const title = document.createElement("span");
  title.className = "message-runtime";
  title.textContent = signal.title || "外部信号";
  const actions = document.createElement("span");
  actions.className = "message-actions";
  const reply = document.createElement("button");
  reply.type = "button";
  reply.className = "message-action input-signal-reply";
  reply.textContent = "↩";
  reply.title = signal.promotedRevisionId ? "继续讨论" : "回应这条输入";
  reply.setAttribute("aria-label", reply.title);
  reply.addEventListener("click", () => {
    selectedSignalId = signal.id;
    input.focus();
    composerState.textContent = "已附上这条观察、发生时间和来源";
  });
  const dismiss = document.createElement("button");
  dismiss.type = "button";
  dismiss.className = "message-action input-signal-dismiss";
  dismiss.title = "从聊天中移除（不影响后续筛选）";
  dismiss.setAttribute("aria-label", dismiss.title);
  const dismissIcon = document.createElement("i");
  dismissIcon.dataset.lucide = "trash-2";
  dismissIcon.setAttribute("aria-hidden", "true");
  dismiss.append(dismissIcon);
  dismiss.addEventListener("click", async () => {
    dismiss.disabled = true;
    try {
      const response = await fetch(`/api/signals/${encodeURIComponent(signal.id)}`, {
        method: "DELETE",
      });
      if (!response.ok) {
        const payload = await response.json().catch(() => ({}));
        throw new Error(payload.error || "无法移除这条外部输入");
      }
      if (selectedSignalId === signal.id) {
        selectedSignalId = null;
        composerState.textContent = "";
      }
      appState.signals = appState.signals.filter((item) => item.id !== signal.id);
      article.remove();
      regroupInputSignals(conversation);
      emptyState.hidden = Boolean(conversation.querySelector(".message"));
    } catch (error) {
      dismiss.disabled = false;
      notifyComposer(error.message);
    }
  });
  actions.append(reply, dismiss);
  renderIcons(actions);
  foot.append(title, actions);
  content.append(meta, body);
  if (signal.kind === "attacker_challenge" && signal.relatedSignalIds?.length) {
    const relation = document.createElement("p");
    relation.className = "attacker-signal-relation";
    relation.textContent = `↳ 回应 ${signal.relatedSignalIds.length} 条外部输入`;
    content.append(relation);
  }
  const isCondensed = signal.presentation === "condensed";
  if (isCondensed && receivedText && receivedText.trim() !== displayText.trim()) {
    const original = document.createElement("details");
    original.className = "input-signal-original";
    const originalLabel = document.createElement("summary");
    originalLabel.textContent = "展开收到的原文";
    const originalBody = document.createElement("div");
    originalBody.className = "input-signal-original-body";
    renderMessageContent(originalBody, {
      content: receivedText,
      parts: [{ type: "markdown", text: receivedText }],
    });
    original.append(originalLabel, originalBody);
    content.append(original);
  }
  const qualification = signal.qualificationNote || signal.qualification_note;
  if (qualification) {
    const note = document.createElement("p");
    note.className = "input-signal-qualification";
    note.textContent = qualification;
    content.append(note);
  }
  if (signal.sources?.length) {
    const sources = document.createElement("details");
    sources.className = "input-signal-sources";
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
    content.append(sources);
  }
  content.append(foot);
  layout.append(avatar, content);
  article.append(layout);
  conversation.append(article);
  regroupInputSignals(conversation);
  if (options.scroll !== false) conversation.scrollTop = conversation.scrollHeight;
  return article;
}

window.addEventListener("input-roles-updated", (event) => {
  for (const role of event.detail?.roles || []) {
    for (const message of conversation.querySelectorAll(
      `.input-signal[data-input-role-id="${CSS.escape(role.id)}"]`,
    )) {
      message.querySelector(".speaker").textContent = role.name;
      applyInputRoleAvatar(message.querySelector(".message-avatar"), role.avatar);
    }
  }
  regroupInputSignals(conversation);
});

function appendManualExplorationReceipt(receipt) {
  if (!receipt?.id) return false;
  if (manualExplorationReceiptIds.has(receipt.id)) return true;
  manualExplorationReceiptIds.add(receipt.id);
  emptyState.hidden = true;

  const notice = document.createElement("article");
  notice.className = "conversation-notice exploration-receipt";
  notice.dataset.receiptId = receipt.id;
  notice.setAttribute("role", "status");

  const completionNotice = manualCompletionNotice(receipt);
  const label = document.createElement("strong");
  label.textContent = completionNotice.label;
  const message = document.createElement("span");
  message.textContent = completionNotice.message;
  const time = document.createElement("time");
  time.dateTime = receipt.completedAt;
  time.textContent = new Date(receipt.completedAt).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  notice.append(label, message, time);
  conversation.append(notice);
  conversation.scrollTop = conversation.scrollHeight;
  return true;
}

function clearResponseWaitIndicators() {
  clearTimeout(responseWaitTimer);
  clearTimeout(responseDelayTimer);
  responseWaitTimer = null;
  responseDelayTimer = null;
}

function beginResponseWait(pending) {
  clearResponseWaitIndicators();
  responseWaitTimer = window.setTimeout(() => {
    if (!busy || activePending !== pending || !pending.isConnected) return;
    pending.querySelector(".message-body").textContent = "正在优先处理你的消息…";
    connectionStatus.textContent = "正在优先处理你的消息";
  }, 1400);
  responseDelayTimer = window.setTimeout(() => {
    if (!busy || activePending !== pending || !pending.isConnected) return;
    pending.querySelector(".message-body").textContent =
      "仍在连接 Codex；可以停止回复。若通信异常，将自动重建连接。";
    connectionStatus.textContent = "仍在等待 Codex";
  }, 12000);
}

function appendConnectionRecoveryNotice(error, retry) {
  const notice = document.createElement("article");
  notice.className = "conversation-notice connection-recovery";
  notice.setAttribute("role", "status");

  const label = document.createElement("strong");
  label.textContent = "回复未完成";
  const message = document.createElement("span");
  message.textContent = error.message || "与 Codex 的通信中断。";
  const actions = document.createElement("div");
  actions.className = "connection-recovery-actions";
  const retryButton = document.createElement("button");
  retryButton.type = "button";
  retryButton.className = "secondary-button";
  retryButton.textContent = "重试";
  const restartButton = document.createElement("button");
  restartButton.type = "button";
  restartButton.className = "secondary-button";
  restartButton.textContent = "重启连接";

  retryButton.addEventListener("click", async () => {
    retryButton.disabled = true;
    restartButton.disabled = true;
    message.textContent = "正在重新发送…";
    try {
      await retry();
      notice.remove();
    } catch (retryError) {
      message.textContent = retryError.message || "重试失败。";
      retryButton.disabled = false;
      restartButton.disabled = false;
    }
  });
  restartButton.addEventListener("click", async () => {
    retryButton.disabled = true;
    restartButton.disabled = true;
    message.textContent = "正在重建 Codex 连接…";
    try {
      const response = await fetch("/api/runtime/recover", { method: "POST" });
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) {
        throw new Error(payload.error || "无法重建 Codex 连接。");
      }
      message.textContent = "连接已重建。现在可以重试刚才的消息。";
      retryButton.disabled = false;
      restartButton.remove();
      notifyComposer("Codex 连接已重建");
    } catch (restartError) {
      message.textContent = restartError.message || "重建连接失败。";
      retryButton.disabled = false;
      restartButton.disabled = false;
    }
  });

  actions.append(retryButton, restartButton);
  notice.append(label, message, actions);
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
    clearResponseWaitIndicators();
  }
}

function setActivity(message, event) {
  clearResponseWaitIndicators();
  const body = message.querySelector(".message-body");
  const foot = message.querySelector(".message-foot");
  if (!body || !foot) return;
  const runtime = foot.querySelector(".message-runtime");
  message.classList.remove("response-placeholder");
  message.classList.add("pending");
  body.textContent = event.label;
  connectionStatus.textContent = "在线";
  setRuntimeStatus(event.label, "working");
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
  clearResponseWaitIndicators();
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
    ? `今日后台消耗 ${today} / ${formatTokens(limit)}`
    : `今日后台消耗 ${today} · 未设上限`;
}

function renderRuntimeStatus() {
  if (busy) return;
  if (appState.profile.status === "calibrating") {
    connectionStatus.textContent = "初始化中";
    setRuntimeStatus("正在初始化对话", "working");
    return;
  }
  connectionStatus.textContent = "在线";
  const exploration = appState.exploration;
  const attacker = appState.attacker;
  const phase = exploration?.phase;
  if (phase === "exploring") {
    const reviewing = exploration.currentReviewCandidateCount
      ? ` · 本轮复核 ${exploration.currentReviewCandidateCount} 条`
      : "";
    const candidates = exploration.pendingCandidateCount
      ? ` · 候选积压 ${exploration.pendingCandidateCount} 条`
      : "";
    setRuntimeStatus(
      `${exploration.currentActivity?.label || "正在主动探索"}${reviewing}${candidates}`,
      "working",
    );
  } else if (attacker?.phase === "reviewing") {
    setRuntimeStatus(
      `正在逆向审视 ${attacker.currentBatchSize || 0} 条外部输入`,
      "working",
    );
  } else if (phase === "quiet_hours") {
    setRuntimeStatus("主动探索处于安静时段");
  } else if (phase === "token_limit") {
    setRuntimeStatus("今日主动探索预算已用尽", "limited");
  } else if (phase === "message_limit") {
    setRuntimeStatus("今日主动消息额度已用尽", "limited");
  } else if (phase === "error") {
    setRuntimeStatus("最近一次探索运行异常", "error");
  } else if (phase === "waiting" && exploration.nextRunAt) {
    const candidates = exploration.pendingCandidateCount
      ? ` · 候选积压 ${exploration.pendingCandidateCount} 条`
      : "";
    const reviewed = exploration.lastReviewedCandidateCount
      ? ` · 最近复核 ${exploration.lastReviewedCandidateCount} 条`
      : "";
    setRuntimeStatus(`下次探索 ${new Date(
      exploration.nextRunAt,
    ).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}${candidates}${reviewed}`);
  } else {
    const reflection = appState.reflection?.runtime || appState.reflection;
    setRuntimeStatus(
      reflection?.phase === "reflecting"
        ? reflection.currentActivity || "正在整理近期对话"
        : "准备就绪",
      reflection?.phase === "reflecting" ? "working" : "idle",
    );
  }
}

function setRuntimeStatus(message, state = "idle") {
  appStatusRuntime.textContent = message;
  appStatusBar.dataset.runtimeState = state;
}

function applyRuntime(payload) {
  appState.identity = payload.identity || appState.identity;
  appState.usage = payload.usage || appState.usage;
  appState.ambient = payload.ambient || appState.ambient;
  appState.driveInput = payload.driveInput || appState.driveInput;
  appState.mailInput = payload.mailInput || appState.mailInput;
  appState.inputRoles = payload.inputRoles || appState.inputRoles;
  appState.audioTranscription =
    payload.audioTranscription || appState.audioTranscription;
  appState.exploration = payload.exploration || appState.exploration;
  appState.attacker = payload.attacker || appState.attacker;
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
  if (payload.signals) {
    appState.signals = payload.signals;
    for (const signal of payload.signals) appendInputSignal(signal, { scroll: false });
  }
  renderUsage();
  renderRuntimeStatus();
  identityUi.render();
  inputRoleUi.render();
  reflectionUi.renderRuntime();
  reconciliationUi.runtimeUpdated();
  explorationUi.runtimeUpdated();
  composerContextUi.configUpdated();
  permissionUi.render();
  voiceInput.configUpdated();
  turnDispositionUi.applyAll(payload.turnDispositions);
}

async function bootstrap() {
  try {
    const response = await fetch("/api/bootstrap");
    if (!response.ok) throw new Error("无法载入当前会话。");
    const state = await response.json();
    Object.assign(appState, state);
    const timeline = [
      ...state.messages.map((message) => ({ kind: "message", at: message.at, value: message })),
      ...(state.signals || []).map((signal) => ({
        kind: "signal",
        at: signal.observedAt,
        value: signal,
      })),
    ].sort((left, right) => String(left.at).localeCompare(String(right.at)));
    timeline.forEach((item) => {
      if (item.kind === "signal") appendInputSignal(item.value, { scroll: false });
      else appendMessage(item.value, { scroll: false });
    });
    turnDispositionUi.applyAll(state.turnDispositions);
    messageSync.completeBootstrap(state.messages);
    try {
      await ephemeralDiscussionUi.restore();
    } catch (error) {
      notifyComposer(`独立讨论状态不可用：${error.message}`);
    }
    // Opening a refreshed conversation should resume at its current edge. The
    // scroll control still preserves a deliberate manual scroll during the
    // rest of the session.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        conversation.scrollTop = conversation.scrollHeight;
        updateScrollToLatestControl();
      });
    });
    memorySize.textContent = formatMemorySize(state.memoryChars);
    renderUsage();
    renderRuntimeStatus();
    identityUi.render();
    inputRoleUi.render();
    settingsUi.render();
    voiceInput.configUpdated();
    explorationUi.runtimeUpdated();
    composerContextUi.configUpdated();
    composerContextUi.warm();
    reflectionUi.render();
    reconciliationUi.render();
    profileUi.render();
    permissionUi.render();
    messageSync.start();
    requestAnimationFrame(updateScrollToLatestControl);
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
  signalId = selectedSignalId,
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
      signalId,
    );
    return;
  }
  const signal = appState.signals.find((item) => item.id === signalId) || null;
  const localEntry = localUserEntry(text, images, quotes, topic, signal);
  const outgoing = appendMessage(localEntry, { deliveryState: "pending" });
  activeOutgoing = [outgoing];
  const pending = appendMessage(
    {
      role: "assistant",
      at: new Date().toISOString(),
      content: "正在为你的消息准备回复…",
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
  beginResponseWait(pending);

  try {
    const response = await fetch("/api/chat", {
      method: "POST",
      body: chatBody(text, images, minimumLane, quotes, topic, codexTaskIds, signalId),
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
    const failedOutgoing = [...activeOutgoing];
    for (const message of failedOutgoing) {
      messageActions.update(message, null, {
        deliveryState: "failed",
        failureReason: error.message,
      });
    }
    appendConnectionRecoveryNotice(error, async () => {
      const messages = await Promise.all(
        failedOutgoing.map(async (message) => ({
          message,
          entry: messageActions.entryFor(message),
        })),
      );
      const first = messages[0]?.entry;
      if (!first) throw new Error("找不到可重试的消息。");
      for (const { message, entry } of messages) {
        if (entry?.revisionId) await retractMessage(message, entry);
        else message.remove();
      }
      await sendMessage(
        first.content || "",
        await recoverImages(first),
        minimumLane,
        extractQuotes(first),
        extractTopic(first),
        codexTaskIds,
        signalId,
      );
    });
  } finally {
    activeOutgoing = [];
    activePending = null;
    signalTyping(false);
    setBusy(false);
    if (!composer.hidden) input.focus();
    if (selectedSignalId === signalId) selectedSignalId = null;
  }
}

function clearTemporaryDiscussionMessages() {
  for (const message of temporaryDiscussionConversation.querySelectorAll(
    ".temporary-discussion-message",
  )) {
    message.remove();
  }
  temporaryDiscussionEmpty.hidden = false;
}

function renderTemporaryDiscussionSnapshot(snapshot) {
  clearTemporaryDiscussionMessages();
  for (const turn of snapshot?.turns || []) {
    appendMessage(
      {
        role: turn.role,
        at: turn.at,
        content: turn.content,
        parts: [{ type: "markdown", text: turn.content }],
      },
      {
        temporary: true,
        scroll: false,
        container: temporaryDiscussionConversation,
      },
    );
  }
  if (snapshot?.turns?.length) {
    temporaryDiscussionConversation.scrollTop =
      temporaryDiscussionConversation.scrollHeight;
  }
}

function renderTemporaryDiscussionPending(text) {
  appendMessage(localUserEntry(text, [], [], null), {
    temporary: true,
    container: temporaryDiscussionConversation,
  });
  appendMessage(
    {
      role: "assistant",
      at: new Date().toISOString(),
      content: "正在回应独立讨论…",
    },
    {
      temporary: true,
      pending: true,
      container: temporaryDiscussionConversation,
    },
  );
}

async function appendToActiveResponse(
  text,
  images,
  minimumLane,
  quotes,
  topic,
  codexTaskIds = [],
  signalId = selectedSignalId,
) {
  const signal = appState.signals.find((item) => item.id === signalId) || null;
  const outgoing = appendMessage(localUserEntry(text, images, quotes, topic, signal), {
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
      body: chatBody(text, images, minimumLane, quotes, topic, codexTaskIds, signalId),
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

function localUserEntry(text, images, quotes, topic, signal = null) {
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
      ...(signal
        ? [
            {
              type: "externalInput",
              input: {
                sourceRevisionId: signal.promotedRevisionId || "",
                actorName: signal.actor?.name || "外部输入",
                title: signal.title || "外部输入",
                observedAt:
                  signal.observedAt || signal.observed_at || new Date().toISOString(),
                excerpt:
                  signal.content ||
                  signal.receivedText ||
                  signal.received_text ||
                  signal.summary ||
                  "",
                sourceCount: signal.sources?.length || 0,
              },
            },
          ]
        : []),
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
  signalId = null,
) {
  const body = new FormData();
  body.append("message", text);
  body.append("computeLane", minimumLane);
  if (topic?.id) body.append("topicId", topic.id);
  for (const quote of quotes) {
    const draft = quoteDraft(quote);
    if (draft) body.append("quote", JSON.stringify(draft));
  }
  for (const image of images) body.append("image", image.file, image.file.name);
  for (const taskId of codexTaskIds) body.append("codexTaskId", taskId);
  if (signalId) body.append("signalId", signalId);
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
  appStatusBar.dataset.notice = message ? "active" : "idle";
  composerNoticeTimer = window.setTimeout(() => {
    if (composerState.textContent === message) composerState.textContent = "";
    if (!composerState.textContent) appStatusBar.dataset.notice = "idle";
  }, 2200);
}

function setPersistentStatus(message) {
  clearTimeout(composerNoticeTimer);
  composerState.textContent = message;
  appStatusBar.dataset.notice = message ? "active" : "idle";
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
  const minimumLane = computeMode.value;
  const codexTaskIds = composerContextUi.consume();
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
    const payload = await fetch("/api/chat/interrupt", { method: "POST" }).then(
      async (response) => {
        const result = await response.json();
        if (!response.ok) throw new Error(result.error || "无法停止回复");
        return result;
      },
    );
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
  .addEventListener("click", () => usageUi.open());

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
