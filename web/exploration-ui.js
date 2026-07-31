import {
  formatDuration,
  formatTokens,
  responseJson,
} from "/presentation.js";
import { renderRichText } from "/rich-text.js";

export function initExplorationUi(state) {
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
    if (!payload.runs.length) {
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
    if (triggering || queued || state.exploration?.phase === "exploring") {
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
    const exploring = state.exploration?.phase === "exploring";
    const stateLabel = exploring
      ? "探索中"
      : queued
        ? "探索已加入队列"
        : triggering
          ? "正在检查探索条件"
          : "立即进行一次探索";
    quickRun.disabled =
      !state.autonomyPermitted || triggering || queued || exploring;
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
      const runAt = state.exploration?.lastRunAt || null;
      if (state.exploration?.phase === "exploring") queued = false;
      renderQuickRun();
      if (!dialog.open) return;
      status.textContent = currentStatus(state.exploration);
      if (runAt && runAt !== lastLoadedRunAt) load();
    },
  };
}

function renderRun(run) {
  const article = document.createElement("article");
  article.className = "exploration-run";
  article.dataset.outcome = run.surfaced ? "messaged" : "silent";

  const header = document.createElement("header");
  const identity = document.createElement("span");
  const outcome = document.createElement("strong");
  const time = document.createElement("time");
  outcome.textContent = run.surfaced ? "发出了一条消息" : "完成，决定不打扰";
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

  if (run.message) {
    const message = document.createElement("div");
    message.className = "exploration-result rich-text";
    renderRichText(message, run.message);
    article.append(message);
  } else {
    const silent = document.createElement("p");
    silent.className = "exploration-silent";
    silent.textContent = run.detailsRetained
      ? "模型完成了检索和判断，但没有发现值得现在打扰你的信号。"
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
    summary.textContent = `判断过程摘要 · ${processItems.length} 条`;
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
  const modelText = run.modelRuns
    .map((model) => `${model.displayName || model.model} · ${model.effort}`)
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

function currentStatus(exploration) {
  if (!exploration) return "状态未知";
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

function triggerLabel(trigger) {
  return (
    {
      scheduled: "定时",
      manual: "手动",
      conversation_hunch: "对话触发",
      deferred_follow_up: "延迟续话",
    }[trigger] || ""
  );
}

function cleanProcessText(value) {
  return value.replace(/^\*\*(.+)\*\*$/, "$1");
}
