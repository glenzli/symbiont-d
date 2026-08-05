import {
  formatDuration,
  formatTokens,
  responseJson,
} from "/presentation.js";
import { renderRichText } from "/rich-text.js";
import {
  manualCompletionSince,
  manualRunLabel,
  manualRunPending,
  unpresentedManualCompletions,
} from "/exploration-receipt.js";

const PENDING_MANUAL_KEY = "symbiont-d:pending-manual-exploration:v1";

export function initExplorationUi(state, { announceManualCompletion } = {}) {
  const dialog = document.querySelector("#exploration-dialog");
  const status = document.querySelector("#exploration-dialog-status");
  const history = document.querySelector("#exploration-history");
  const quickRun = document.querySelector("#run-exploration-quick");
  const budgetDialog = document.querySelector("#exploration-budget-dialog");
  const budgetUsed = document.querySelector("#exploration-budget-used");
  const budgetLimit = document.querySelector("#exploration-budget-limit");
  const cancelOverride = document.querySelector("#cancel-exploration-override");
  const confirmOverride = document.querySelector("#confirm-exploration-override");
  let lastLoadedRunAt = null;
  let triggering = false;
  let queued = false;
  let budgetResolver = null;
  let pendingManualRequestId = readPendingManualRequest();
  const acknowledgingReceiptIds = new Set();

  async function load() {
    status.textContent = "正在读取";
    history.textContent = "";
    try {
      const payload = await responseJson(
        await fetch("/api/exploration/recent", { cache: "no-store" }),
        "读取最近探索失败",
      );
      state.exploration = payload.exploration;
      lastLoadedRunAt = payload.exploration?.lastRunAt || null;
      render(payload);
    } catch (error) {
      status.textContent = "读取失败";
      history.textContent = error.message;
    }
  }

  function render(payload) {
    status.textContent = currentStatus(payload.exploration);
    history.replaceChildren();
    if (payload.exploration?.lastError) {
      const error = document.createElement("p");
      error.className = "exploration-error";
      error.textContent = `最近异常：${payload.exploration.lastError}`;
      history.append(error);
    }
    if (payload.candidates?.length) {
      history.append(renderCandidatePool(payload.candidates));
    }
    if (payload.intents?.length) history.append(renderIntentLog(payload.intents));
    if (!payload.runs.length && !payload.candidates?.length) {
      const empty = document.createElement("p");
      empty.className = "exploration-empty";
      empty.textContent = "还没有完成过自主探索。";
      history.append(empty);
      return;
    }
    for (const run of payload.runs) history.append(renderRun(run));
  }

  function open() {
    dialog.showModal();
    load();
  }

  async function trigger() {
    if (triggering || queued || manualRunPending(state.exploration)) {
      return { accepted: false, alreadyQueued: true };
    }
    triggering = true;
    renderQuickRun();
    try {
      let payload = await requestExploration(false);
      if (payload.requiresConfirmation) {
        const confirmed = await confirmBudget(payload);
        if (!confirmed) return { accepted: false, canceled: true };
        payload = await requestExploration(true);
      }
      if (payload.accepted) {
        pendingManualRequestId = payload.requestId;
        rememberPendingManualRequest(pendingManualRequestId);
        queued = true;
        window.setTimeout(() => {
          queued = false;
          renderQuickRun();
        }, 5_000);
      }
      return payload;
    } finally {
      triggering = false;
      renderQuickRun();
    }
  }

  async function requestExploration(overrideTokenLimit) {
    const query = overrideTokenLimit ? "?overrideTokenLimit=true" : "";
    return responseJson(
      await fetch(`/api/exploration/run${query}`, { method: "POST" }),
      "无法开始探索",
    );
  }

  async function acknowledgeManualCompletion(completion) {
    if (!completion?.id || acknowledgingReceiptIds.has(completion.id)) return;
    acknowledgingReceiptIds.add(completion.id);
    try {
      const receipt = await responseJson(
        await fetch(
          `/api/exploration/receipts/${encodeURIComponent(completion.id)}/ack`,
          { method: "POST" },
        ),
        "确认探索通知失败",
      );
      const projected = state.exploration?.manualReceipts?.find(
        (candidate) => candidate.id === receipt.id,
      );
      if (projected) projected.presentedAt = receipt.presentedAt;
    } catch {
      // Keep the durable receipt unacknowledged so the next runtime poll retries.
    } finally {
      acknowledgingReceiptIds.delete(completion.id);
    }
  }

  function confirmBudget(payload) {
    budgetUsed.textContent = formatTokens(payload.autonomousTokensToday || 0);
    budgetLimit.textContent = formatTokens(payload.dailyTokenLimit || 0);
    budgetDialog.showModal();
    return new Promise((resolve) => {
      budgetResolver = resolve;
    });
  }

  function resolveBudget(confirmed) {
    if (budgetDialog.open) budgetDialog.close();
    const resolve = budgetResolver;
    budgetResolver = null;
    resolve?.(confirmed);
  }

  function renderQuickRun() {
    const manualRun = state.exploration?.manualRun;
    const manualPending = manualRunPending(state.exploration);
    const exploring = manualRun?.status === "exploring";
    const stateLabel = manualPending
      ? manualRunLabel(manualRun)
      : queued
        ? "探索已加入队列"
        : triggering
          ? "正在检查探索条件"
          : "立即进行一次探索";
    quickRun.disabled =
      !state.autonomyPermitted || triggering || queued || manualPending;
    quickRun.dataset.state = exploring
      ? "exploring"
      : queued || triggering
        ? "pending"
        : "ready";
    quickRun.title = stateLabel;
    quickRun.dataset.tooltip = stateLabel;
    quickRun.setAttribute("aria-label", stateLabel);
  }

  document
    .querySelector("#open-explorations")
    .addEventListener("click", open);
  quickRun.addEventListener("click", () => {
    trigger().catch((error) => {
      status.textContent = error.message;
      if (!dialog.open) dialog.showModal();
    });
  });
  cancelOverride.addEventListener("click", () => resolveBudget(false));
  confirmOverride.addEventListener("click", () => resolveBudget(true));
  budgetDialog.addEventListener("cancel", (event) => {
    event.preventDefault();
    resolveBudget(false);
  });
  renderQuickRun();

  return {
    trigger,
    runtimeUpdated() {
      const exploration = state.exploration;
      const runAt = exploration?.lastRunAt || null;
      if (manualRunPending(exploration)) queued = false;
      const completions = unpresentedManualCompletions(exploration);
      if (
        pendingManualRequestId &&
        !completions.some((completion) => completion.id === pendingManualRequestId)
      ) {
        const legacyCompletion = manualCompletionSince(
          exploration,
          pendingManualRequestId,
        );
        if (legacyCompletion) completions.push(legacyCompletion);
      }
      for (const completion of completions) {
        if (completion.id === pendingManualRequestId) {
          pendingManualRequestId = null;
          rememberPendingManualRequest(null);
        }
        if (announceManualCompletion?.(completion)) {
          void acknowledgeManualCompletion(completion);
        }
      }
      renderQuickRun();
      if (!dialog.open) return;
      status.textContent = currentStatus(exploration);
      if (runAt && runAt !== lastLoadedRunAt) load();
    },
  };
}

function renderCandidatePool(candidates) {
  const section = document.createElement("section");
  section.className = "exploration-candidate-pool";
  const header = document.createElement("header");
  const title = document.createElement("strong");
  title.textContent = `最近感知候选 · ${candidates.length} 条`;
  const note = document.createElement("span");
  note.textContent = "临时信息，不写入记忆；下一次感知会替换";
  header.append(title, note);
  const list = document.createElement("ol");
  for (const candidate of candidates) {
    const item = document.createElement("li");
    const itemHeader = document.createElement("header");
    const candidateTitle = document.createElement("strong");
    candidateTitle.textContent = candidate.title;
    const sourceClass = document.createElement("span");
    sourceClass.textContent = sourceClassLabel(candidate.sourceClass);
    itemHeader.append(candidateTitle, sourceClass);
    const summary = document.createElement("p");
    summary.textContent = candidate.summary;
    item.append(itemHeader, summary);
    if (candidate.possibleConnection) {
      const connection = document.createElement("p");
      connection.className = "candidate-connection";
      connection.textContent = `可能关联：${candidate.possibleConnection}`;
      item.append(connection);
    }
    if (candidate.sources?.length) {
      const sources = document.createElement("ul");
      sources.className = "candidate-sources";
      for (const source of candidate.sources) {
        const sourceItem = document.createElement("li");
        const url = safeHttpUrl(source.url);
        if (url) {
          const link = document.createElement("a");
          link.href = url;
          link.target = "_blank";
          link.rel = "noreferrer";
          link.textContent = source.detail || url;
          sourceItem.append(link);
        } else {
          sourceItem.textContent = source.detail || source.url;
        }
        sources.append(sourceItem);
      }
      item.append(sources);
    }
    list.append(item);
  }
  section.append(header, list);
  return section;
}

function safeHttpUrl(value) {
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

function sourceClassLabel(value) {
  return {
    research: "研究与方法",
    products_and_tools: "产品、评测与使用",
    projects_and_ecosystems: "项目与生态",
    institutions_and_policy: "机构与政策",
    industry_and_markets: "产业与市场",
    culture_and_ideas: "文化与观念",
    open_discovery: "开放发现",
  }[value] || "开放发现";
}

function renderRun(run) {
  const article = document.createElement("article");
  article.className = "exploration-run";
  article.dataset.outcome = run.surfaced ? "messaged" : "silent";
  const sensingOnly = run.scope === "sensing";

  const header = document.createElement("header");
  const identity = document.createElement("span");
  const outcome = document.createElement("strong");
  const time = document.createElement("time");
  outcome.textContent =
    run.status !== "completed"
      ? sensingOnly
        ? "感知未完整结束"
        : "探索未完整结束"
      : run.surfaced
        ? run.outreachKind === "discussion"
          ? "发起了一个讨论"
          : run.outreachKind === "note"
            ? "留了一条新消息"
            : "发起了一次介入"
        : sensingOnly
          ? "完成感知，未进入深入复核"
          : "完成，决定不打扰";
  time.dateTime = run.completedAt;
  time.textContent = new Date(run.completedAt).toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
  identity.append(outcome, time);

  const trace = document.createElement("button");
  trace.type = "button";
  trace.className = "trace-button";
  trace.title = "查看完整执行轨迹";
  trace.setAttribute("aria-label", "查看完整执行轨迹");
  trace.dataset.traceId = run.traceId;
  trace.textContent = "⎇";
  header.append(identity, trace);
  article.append(header);

  if (run.focus) {
    const focus = document.createElement("section");
    focus.className = "exploration-focus";
    const label = document.createElement("strong");
    const title = document.createElement("p");
    label.textContent = sensingOnly ? "这次感知了什么" : "这次看了什么";
    title.textContent = run.focus.title;
    focus.append(label, title);
    if (run.focus.detail) {
      const detail = document.createElement("p");
      detail.textContent = run.focus.detail;
      focus.append(detail);
    }
    article.append(focus);
  }

  if (run.message) {
    const message = document.createElement("div");
    message.className = "exploration-result rich-text";
    renderRichText(message, run.message);
    article.append(message);
  } else {
    const silent = document.createElement("p");
    silent.className = "exploration-silent";
    silent.textContent = run.detailsRetained
      ? sensingOnly
        ? run.sensingCandidateCount
          ? `形成了 ${run.sensingCandidateCount} 条临时候选，但本轮没有进入深入复核。`
          : "完成了外部信息感知，但没有形成值得进入深入复核的候选。"
        : "模型完成了检索和判断，但没有发现值得介入或留给你的新信号。"
      : sensingOnly
        ? "本次感知没有进入深入复核；详细过程已过期。"
        : "本次没有发出消息；详细判断过程已过期。";
    article.append(silent);
  }

  const processItems = [
    ...run.reasoningSummaries.map((text) => ({ kind: "判断", text })),
    ...run.searchQueries.map((text) => ({ kind: "检索", text })),
  ];
  if (processItems.length) {
    const details = document.createElement("details");
    details.className = "exploration-process";
    const summary = document.createElement("summary");
    summary.textContent = `${sensingOnly ? "感知" : "判断"}过程摘要 · ${processItems.length} 条`;
    const list = document.createElement("ol");
    for (const item of processItems) {
      const entry = document.createElement("li");
      const kind = document.createElement("span");
      const text = document.createElement("span");
      kind.textContent = item.kind;
      text.textContent = cleanProcessText(item.text);
      entry.append(kind, text);
      list.append(entry);
    }
    details.append(summary, list);
    article.append(details);
  }

  const footer = document.createElement("footer");
  const stageLabels = {
    sense: "感知",
    scout: "侦察",
    review: "复核",
    explore: "探索",
  };
  const modelText = run.modelRuns
    .map(
      (model) =>
        `${stageLabels[model.stage] || "探索"} ${model.displayName || model.model} · ${model.effort}`,
    )
    .join(" → ");
  const pcp = run.pcpRecallCalls
    ? ` · PCP 召回 ${run.pcpRecallCalls}`
    : "";
  footer.textContent = `${modelText} · ${formatTokens(
    run.totalTokens,
  )} · ${formatDuration(run.durationMs)} · 网页检索 ${run.webSearches}${pcp}`;
  article.append(footer);
  return article;
}

function renderIntentLog(intents) {
  const details = document.createElement("details");
  details.className = "exploration-intent-log";
  const summary = document.createElement("summary");
  const active = intents.filter((intent) =>
    ["queued", "exploring"].includes(intent.status),
  ).length;
  summary.textContent = `思考触发 · ${intents.length}${active ? ` · ${active} 个待处理` : ""}`;
  const list = document.createElement("ol");
  for (const intent of intents) {
    const item = document.createElement("li");
    item.dataset.status = intent.status;
    const header = document.createElement("header");
    const status = document.createElement("strong");
    const time = document.createElement("time");
    status.textContent = intentStatusLabel(intent.status);
    time.dateTime = intent.completedAt || intent.requestedAt;
    time.textContent = new Date(time.dateTime).toLocaleString([], {
      month: "numeric",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
    header.append(status, time);
    if (intent.traceId) {
      const trace = document.createElement("button");
      trace.type = "button";
      trace.className = "trace-button";
      trace.title = "查看这次探索的完整执行轨迹";
      trace.setAttribute("aria-label", "查看这次探索的完整执行轨迹");
      trace.dataset.traceId = intent.traceId;
      trace.textContent = "⎇";
      header.append(trace);
    }
    const question = document.createElement("p");
    question.textContent = intent.question;
    const rationale = document.createElement("p");
    rationale.textContent = intent.whyNow;
    item.append(header, question, rationale);
    list.append(item);
  }
  details.append(summary, list);
  return details;
}

function intentStatusLabel(status) {
  return (
    {
      queued: "等待探索",
      exploring: "正在探索",
      silent: "完成，保持安静",
      messaged: "完成，已发消息",
      superseded: "后续对话已使它失效",
      failed: "探索失败",
    }[status] || status
  );
}

function currentStatus(exploration) {
  if (!exploration) return "状态未知";
  if (manualRunPending(exploration)) {
    return manualRunLabel(exploration.manualRun);
  }
  if (exploration.phase === "exploring") {
    return exploration.currentActivity?.label || "正在自主探索";
  }
  if (exploration.phase === "error") {
    return "最近一次探索运行异常";
  }
  if (exploration.lastRunAt) {
    const trigger = triggerLabel(exploration.lastTrigger);
    return `上次完成于 ${new Date(exploration.lastRunAt).toLocaleString()}${trigger ? ` · ${trigger}` : ""}`;
  }
  if (exploration.phase === "disabled") return "自主探索已关闭";
  if (exploration.phase === "needs_setup") return "等待完成初始化";
  return "等待首次探索";
}

function readPendingManualRequest() {
  try {
    return window.sessionStorage.getItem(PENDING_MANUAL_KEY);
  } catch {
    return null;
  }
}

function rememberPendingManualRequest(requestId) {
  try {
    if (requestId) window.sessionStorage.setItem(PENDING_MANUAL_KEY, requestId);
    else window.sessionStorage.removeItem(PENDING_MANUAL_KEY);
  } catch {
    // Session persistence is only a reload aid; live polling remains authoritative.
  }
}

function triggerLabel(trigger) {
  return (
    {
      scheduled: "定时",
      manual: "手动",
      thought_intent: "思考触发",
      deferred_follow_up: "延迟续话",
    }[trigger] || ""
  );
}

function cleanProcessText(value) {
  return value.replace(/^\*\*(.+)\*\*$/, "$1");
}
