import { formatTokens, responseJson } from "/presentation.js";
import { renderIcons } from "/icons.js";

export function initReconciliationUi(state) {
  const dialog = document.querySelector("#reconciliation-dialog");
  const openButton = document.querySelector("#open-reconciliation");
  const status = document.querySelector("#reconciliation-dialog-status");
  const candidateCount = document.querySelector(
    "#reconciliation-candidate-count",
  );
  const lastRun = document.querySelector("#reconciliation-last-run");
  const indexCount = document.querySelector("#memory-index-count");
  const indexLastSync = document.querySelector("#memory-index-last-sync");
  const previewButton = document.querySelector("#preview-reconciliation");
  const applyButton = document.querySelector("#apply-reconciliation");
  const content = document.querySelector("#reconciliation-content");
  const budgetDialog = document.querySelector("#reconciliation-budget-dialog");
  const budgetUsed = document.querySelector("#reconciliation-budget-used");
  const budgetLimit = document.querySelector("#reconciliation-budget-limit");
  let previousPhase = null;
  let loading = false;

  openButton.addEventListener("click", async () => {
    if (!dialog.open) dialog.showModal();
    await load();
  });
  previewButton.addEventListener("click", () => triggerPreview());
  applyButton.addEventListener("click", () => applyLatest(false));
  document
    .querySelector("#cancel-reconciliation-override")
    .addEventListener("click", () => budgetDialog.close());
  document
    .querySelector("#confirm-reconciliation-override")
    .addEventListener("click", () => {
      budgetDialog.close();
      applyLatest(true);
    });

  async function load() {
    if (loading) return;
    loading = true;
    try {
      state.reconciliation = await responseJson(
        await fetch("/api/reconciliation"),
        "无法读取记忆整理状态",
      );
      render();
    } catch (error) {
      status.textContent = error.message;
    } finally {
      loading = false;
    }
  }

  async function triggerPreview() {
    previewButton.disabled = true;
    status.textContent = "正在加入检查队列";
    try {
      const result = await responseJson(
        await fetch("/api/reconciliation/preview", { method: "POST" }),
        "无法开始检查",
      );
      if (!result.accepted) throw new Error("整理任务正在运行或队列已满");
    } catch (error) {
      status.textContent = error.message;
      previewButton.disabled = false;
    }
  }

  async function applyLatest(overrideTokenLimit) {
    const preview = state.reconciliation?.latestPreview;
    if (!preview) return;
    applyButton.disabled = true;
    status.textContent = "正在加入应用队列";
    try {
      const result = await responseJson(
        await fetch(
          `/api/reconciliation/apply/${encodeURIComponent(preview.id)}` +
            `?overrideTokenLimit=${overrideTokenLimit}`,
          { method: "POST" },
        ),
        "无法应用建议",
      );
      if (result.requiresConfirmation) {
        budgetUsed.textContent = formatTokens(result.backgroundTokensToday || 0);
        budgetLimit.textContent = formatTokens(result.dailyTokenLimit || 0);
        budgetDialog.showModal();
        render();
        return;
      }
      if (!result.accepted) throw new Error("整理任务正在运行或队列已满");
    } catch (error) {
      status.textContent = error.message;
      render();
    }
  }

  function render() {
    const snapshot = state.reconciliation;
    if (!snapshot) return;
    const runtime = snapshot.runtime || snapshot;
    const runs = snapshot.recentRuns || [];
    const preview = snapshot.latestPreview;
    const applied = preview
      ? runs.some(
          (run) =>
            run.mode === "apply" &&
            run.status === "completed" &&
            run.previewRunId === preview.id,
        )
      : false;
    status.textContent = runtimeText(runtime);
    candidateCount.textContent = `${runtime.candidateCount || 0} 页`;
    lastRun.textContent = runtime.lastRunAt
      ? formatDate(runtime.lastRunAt)
      : "尚未运行";
    const memoryIndex = state.memoryIndex;
    indexCount.textContent = `${memoryIndex?.episodePages || 0} 页`;
    indexLastSync.textContent = memoryIndex?.lastSyncAt
      ? memoryIndex.phase === "error"
        ? "校准异常"
        : formatDate(memoryIndex.lastSyncAt)
      : "尚未运行";
    const running = ["previewing", "applying"].includes(runtime.phase);
    previewButton.disabled = running;
    applyButton.disabled =
      running || !preview?.proposals?.length || applied || preview.status !== "completed";
    renderContent(snapshot, applied);
  }

  function renderContent(snapshot, applied) {
    content.replaceChildren();
    const preview = snapshot.latestPreview;
    const proposalSection = document.createElement("section");
    proposalSection.className = "reconciliation-section";
    proposalSection.append(sectionTitle(applied ? "最近检查建议" : "待应用建议"));
    if (!preview?.proposals?.length) {
      proposalSection.append(empty("当前没有需要调整的记忆结构。"));
    } else {
      const summary = document.createElement("p");
      summary.className = "reconciliation-summary";
      summary.textContent = preview.summary || "检查已完成";
      proposalSection.append(summary);
      for (const proposal of preview.proposals) {
        proposalSection.append(renderProposal(proposal));
      }
      if (applied) {
        const note = document.createElement("small");
        note.className = "reconciliation-applied";
        note.textContent = "这批建议已处理，实际变更见运行记录";
        proposalSection.append(note);
      }
    }
    content.append(proposalSection);

    const runSection = document.createElement("section");
    runSection.className = "reconciliation-section";
    runSection.append(sectionTitle("近期运行"));
    if (!snapshot.recentRuns?.length) {
      runSection.append(empty("还没有整理记录。"));
    } else {
      for (const run of snapshot.recentRuns) runSection.append(renderRun(run));
    }
    content.append(runSection);
  }

  function runtimeUpdated() {
    const runtime = state.reconciliation?.runtime || state.reconciliation;
    if (!runtime) return;
    const wasRunning = ["previewing", "applying"].includes(previousPhase);
    const running = ["previewing", "applying"].includes(runtime.phase);
    previousPhase = runtime.phase;
    render();
    if (dialog.open && wasRunning && !running) load();
  }

  return { render, runtimeUpdated };
}

function renderProposal(proposal) {
  const item = document.createElement("article");
  item.className = "reconciliation-proposal";
  const header = document.createElement("header");
  const kind = document.createElement("small");
  const subject = document.createElement("strong");
  kind.textContent = proposalKind(proposal.action);
  subject.textContent = proposal.subject;
  header.append(kind, subject);
  const reason = document.createElement("p");
  reason.textContent = proposal.reason;
  const sources = document.createElement("small");
  sources.textContent = `${proposal.revisionIds?.length || 0} 个 Revision`;
  item.append(header, reason, sources);
  return item;
}

function renderRun(run) {
  const item = document.createElement("article");
  item.className = "reconciliation-run";
  const header = document.createElement("header");
  const title = document.createElement("strong");
  const meta = document.createElement("small");
  title.textContent = `${run.mode === "apply" ? "应用" : "检查"} · ${runStatus(run.status)}`;
  meta.textContent = [
    formatDate(run.startedAt),
    run.model,
    formatTokens(run.totalTokens || 0),
  ]
    .filter(Boolean)
    .join(" · ");
  header.append(title, meta);
  if (run.traceId) {
    const trace = document.createElement("button");
    trace.type = "button";
    trace.className = "trace-button";
    trace.title = "查看执行轨迹";
    trace.dataset.traceId = run.traceId;
    trace.textContent = "轨迹";
    header.append(trace);
  }
  const summary = document.createElement("p");
  summary.textContent = run.summary || run.error || "没有生成说明";
  const result = document.createElement("small");
  result.textContent = run.actions?.length
    ? `${run.actions.length} 个实际变更`
    : `${run.proposals?.length || 0} 条建议`;
  item.append(header, summary, result);
  return item;
}

function sectionTitle(text) {
  const title = document.createElement("h3");
  title.textContent = text;
  return title;
}

function empty(text) {
  const item = document.createElement("p");
  item.className = "archive-empty";
  item.textContent = text;
  return item;
}

function runtimeText(runtime) {
  if (runtime.phase === "previewing") return runtime.currentActivity || "正在检查";
  if (runtime.phase === "applying") return runtime.currentActivity || "正在应用";
  if (runtime.phase === "needs_setup") return "等待完成初始化";
  if (runtime.phase === "token_limit") return "今日后台分析预算已用尽";
  if (runtime.phase === "error") return runtime.lastError || "运行异常";
  return runtime.lastSummary || "等待检查";
}

function proposalKind(kind) {
  return {
    classify: "补充分类",
    consolidate: "收敛重复页",
    synthesize: "形成综合页",
    link: "建立关系",
    assess_validity: "检查有效性",
    resummarize: "重写索引摘要",
  }[kind] || kind;
}

function runStatus(status) {
  return {
    completed: "完成",
    running: "运行中",
    interrupted: "已中断",
    error: "异常",
  }[status] || status;
}

function formatDate(value) {
  if (!value) return "";
  return new Date(value).toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
