import { formatDuration, formatTokens } from "/presentation.js";

export function initTraceUi() {
  const dialog = document.querySelector("#trace-dialog");
  const summary = document.querySelector("#trace-summary");
  const content = document.querySelector("#trace-content");

  document.addEventListener("click", (event) => {
    const button =
      event.target instanceof Element
        ? event.target.closest(".trace-button")
        : null;
    if (button?.dataset.traceId) openTrace(button.dataset.traceId);
  });

  async function openTrace(traceId) {
    summary.textContent = "正在读取";
    content.textContent = "";
    dialog.showModal();
    try {
      const response = await fetch(
        `/api/traces/${encodeURIComponent(traceId)}`,
      );
      const payload = await response.json();
      if (!response.ok) throw new Error(payload.error || "无法读取执行轨迹");
      renderTrace(payload);
    } catch (error) {
      summary.textContent = "读取失败";
      content.textContent = error.message;
    }
  }

  function renderTrace(trace) {
    const recallCount =
      trace.pcpRecallCalls ?? countPcpSteps(trace, isPcpRecall);
    const writeCount =
      trace.pcpWriteCalls ?? countPcpSteps(trace, isPcpWrite);
    const reusedRecallCount = countPcpSteps(trace, isReusedPcpRecall);
    const executedRecallCount = Math.max(0, recallCount - reusedRecallCount);
    const reuseSummary = reusedRecallCount
      ? `，${reusedRecallCount} 次重复请求已复用`
      : "";
    summary.textContent = `${trace.runs.length} 个模型运行 · ${trace.eventCount} 个可观察阶段 · ${executedRecallCount} 次召回${reuseSummary} · ${writeCount} 次写入`;
    content.replaceChildren();

    const retention = document.createElement("p");
    retention.className = "trace-retention";
    retention.textContent = `完整明细保留 ${trace.retentionDays} 天或最近 ${trace.retentionInvocations} 次模型运行`;
    content.append(retention);

    if (recallCount > 0) {
      const recall = document.createElement("aside");
      recall.className = "trace-recall-notice";
      const title = document.createElement("strong");
      const description = document.createElement("span");
      title.textContent = `PCP 实际召回 ${executedRecallCount} 次`;
      description.textContent = reusedRecallCount
        ? `模型还提交了 ${reusedRecallCount} 次完全相同的请求，Host 直接复用了本轮前次结果；对应步骤已在下方标记。`
        : "模型在本轮主动搜索或读取了长期上下文；对应步骤已在下方高亮。";
      recall.append(title, description);
      content.append(recall);
    }

    if (!trace.detailsRetained) {
      const expired = document.createElement("p");
      expired.className = "trace-empty";
      expired.textContent = "这次运行的详细轨迹已过期，仅保留调用与用量统计。";
      content.append(expired);
    }

    trace.runs.forEach((run, index) => {
      content.append(renderRun(run, index));
    });
  }
}

function renderRun(run, index) {
  const article = document.createElement("article");
  article.className = "trace-run";
  const header = document.createElement("header");
  const identity = document.createElement("span");
  const model = document.createElement("strong");
  const details = document.createElement("small");
  const id = document.createElement("code");
  model.textContent = `${index + 1}. ${run.displayName || run.model}`;
  details.textContent = `${run.lane} · ${run.effort} · ${formatTokens(
    run.totalTokens,
  )} · ${formatDuration(run.durationMs)}`;
  id.textContent = shortId(run.invocationId);
  identity.append(model, details);
  header.append(identity, id);
  article.append(header);

  const tokenLine = document.createElement("p");
  tokenLine.className = "trace-token-line";
  tokenLine.textContent = `输入 ${formatTokens(run.inputTokens)} · 缓存 ${formatTokens(
    run.cachedInputTokens,
  )} · 输出 ${formatTokens(run.outputTokens)} · 推理 ${formatTokens(
    run.reasoningOutputTokens,
  )}`;
  article.append(tokenLine);

  if (run.context) article.append(renderContext(run.context));

  const timeline = document.createElement("section");
  timeline.className = "trace-timeline";
  const usedToolSteps = new Set();
  for (const event of run.events || []) {
    if (event.kind === "toolCall") {
      const sequence = event.details?.toolSequence;
      const step = run.steps.find((candidate) => candidate.sequence === sequence);
      if (step) {
        usedToolSteps.add(sequence);
        timeline.append(renderTraceStep(step, event.sequence));
        continue;
      }
    }
    timeline.append(renderTraceEvent(event));
  }
  for (const step of run.steps) {
    if (!usedToolSteps.has(step.sequence)) {
      timeline.append(renderTraceStep(step, null));
    }
  }
  if (!timeline.childElementCount) {
    const empty = document.createElement("p");
    empty.className = "trace-empty";
    empty.textContent = "没有保留下来的执行阶段。";
    timeline.append(empty);
  }
  article.append(timeline);
  return article;
}

function renderContext(context) {
  const details = document.createElement("details");
  details.className = "trace-context";
  const summary = document.createElement("summary");
  const title = document.createElement("span");
  const state = document.createElement("span");
  title.textContent = "模型可见上下文";
  state.textContent = `${context.nativeThread.priorTurns} 个既有 turn · ${context.nativeThread.compactionsBefore} 次压缩`;
  summary.append(title, state);

  const body = document.createElement("div");
  body.className = "trace-context-body";
  const notice = document.createElement("p");
  notice.className = "trace-context-notice";
  notice.textContent =
    "这里记录客户端提供的输入、应用上下文和 Working Context。Codex 未暴露内部最终组装的 token 序列。";
  body.append(notice);

  const metadata = document.createElement("dl");
  metadata.className = "trace-context-meta";
  appendMeta(metadata, "Thread", shortId(context.nativeThread.threadId));
  appendMeta(
    metadata,
    "Context window",
    context.nativeThread.modelContextWindow
      ? formatTokens(context.nativeThread.modelContextWindow)
      : "未报告",
  );
  appendMeta(
    metadata,
    "Cursor",
    shortId(context.nativeThread.cursorBefore) || "新线程",
  );
  if (context.workingContext) {
    appendMeta(
      metadata,
      "Bridge",
      `${workingReason(context.workingContext.reason)} · ${context.workingContext.messages.length} 条`,
    );
  }
  body.append(metadata);

  if (context.nativeThread.observableHistoryTail?.length) {
    const historyLabel = context.nativeThread.historyTailTruncated
      ? "Codex 可观察历史尾部（更早部分省略）"
      : "Codex 可观察历史尾部";
    body.append(
      tracePayload(
        historyLabel,
        context.nativeThread.observableHistoryTail,
      ),
    );
  }
  body.append(tracePayload("本轮直接输入", context.input));
  for (const fragment of context.fragments) {
    body.append(tracePayload(fragment.source, fragment.value));
  }
  if (context.workingContext) {
    body.append(tracePayload("Working Context manifest", context.workingContext));
  }
  body.append(
    tracePayload("Thread developer instructions", context.developerInstructions),
  );
  details.append(summary, body);
  return details;
}

function renderTraceEvent(event) {
  const details = document.createElement("details");
  details.className = "trace-step trace-event";
  details.dataset.kind = event.kind;
  const summary = document.createElement("summary");
  const name = document.createElement("span");
  const timing = document.createElement("span");
  name.textContent = `${event.sequence + 1}. ${eventTitle(event.kind)}`;
  timing.textContent = formatClock(event.occurredAt);
  summary.append(name, timing);

  const payload = document.createElement("div");
  payload.className = "trace-payload";
  if (event.kind === "reasoningSummary") {
    payload.append(
      tracePayload(
        "Codex reasoning summary",
        (event.details?.summary || []).join("\n\n"),
      ),
    );
  } else {
    payload.append(tracePayload(event.title, event.details));
  }
  details.append(summary, payload);
  return details;
}

function renderTraceStep(step, eventSequence) {
  const details = document.createElement("details");
  details.className = "trace-step";
  details.dataset.success = String(step.succeeded);
  const reusedFromSequence = deduplicatedFromSequence(step);
  if (reusedFromSequence !== null) details.dataset.deduplicated = "true";
  if (isPcpRecall(step)) details.dataset.pcpAction = "recall";
  else if (isPcpWrite(step)) details.dataset.pcpAction = "write";
  const summary = document.createElement("summary");
  const name = document.createElement("span");
  name.className = "trace-step-name";
  const timing = document.createElement("span");
  const position =
    eventSequence === null ? `工具 ${step.sequence + 1}` : eventSequence + 1;
  const label = document.createElement("span");
  label.textContent = `${position}. ${step.namespace}.${step.tool}`;
  name.append(label);
  if (isPcpRecall(step)) name.append(traceToolBadge("PCP 召回"));
  else if (isPcpWrite(step)) name.append(traceToolBadge("PCP 写入"));
  if (reusedFromSequence !== null) {
    name.append(traceToolBadge(`复用 #${reusedFromSequence + 1}`, "reuse"));
  }
  timing.textContent =
    reusedFromSequence === null
      ? `${step.succeeded ? "完成" : "失败"} · ${formatDuration(step.durationMs)}`
      : `未执行 · ${formatDuration(step.durationMs)}`;
  summary.append(name, timing);

  const payload = document.createElement("div");
  payload.className = "trace-payload";
  payload.append(
    tracePayload("输入", step.arguments),
    tracePayload("输出", step.result),
  );
  details.append(summary, payload);
  return details;
}

function traceToolBadge(label, variant = "") {
  const badge = document.createElement("span");
  badge.className = "trace-tool-badge";
  if (variant) badge.dataset.variant = variant;
  badge.textContent = label;
  return badge;
}

function countPcpSteps(trace, predicate) {
  return trace.runs.reduce(
    (count, run) =>
      count + (run.steps || []).filter((step) => predicate(step)).length,
    0,
  );
}

function isPcpRecall(step) {
  return (
    step.namespace === "pcp" &&
    ["browse_index", "search_pages", "read_pages"].includes(step.tool)
  );
}

function isReusedPcpRecall(step) {
  return isPcpRecall(step) && deduplicatedFromSequence(step) !== null;
}

function deduplicatedFromSequence(step) {
  const sequence = step.result?._symbiontTrace?.reusedFromSequence;
  return Number.isInteger(sequence) ? sequence : null;
}

function isPcpWrite(step) {
  return (
    step.namespace === "pcp" &&
    ["assess_validity", "write_summary", "write_page", "revise_page", "link_pages"].includes(step.tool)
  );
}

function tracePayload(label, value) {
  const details = document.createElement("details");
  details.className = "trace-raw";
  const summary = document.createElement("summary");
  const pre = document.createElement("pre");
  summary.textContent = label;
  pre.textContent =
    typeof value === "string" ? value : JSON.stringify(value, null, 2);
  details.append(summary, pre);
  return details;
}

function appendMeta(parent, term, description) {
  const dt = document.createElement("dt");
  const dd = document.createElement("dd");
  dt.textContent = term;
  dd.textContent = description;
  parent.append(dt, dd);
}

function workingReason(value) {
  const reasons = {
    upToDate: "原生线程连续",
    threadStart: "新线程恢复",
    missingEvents: "补入缺失事件",
    cursorOutsideWindow: "游标超出近期窗口",
  };
  return reasons[value] || value;
}

function eventTitle(kind) {
  const titles = {
    reasoningSummary: "模型摘要",
    webSearch: "网页检索",
    contextCompaction: "上下文压缩",
    threadRollover: "原生线程换页",
    modelReroute: "模型改道",
    permissionRequest: "权限请求",
    permissionResolution: "权限决定",
    turnInterrupted: "用户输入中断",
    agentMessage: "最终回复",
  };
  return titles[kind] || kind;
}

function formatClock(value) {
  if (!value) return "";
  return new Date(value).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function shortId(value) {
  if (!value || value.length < 20) return value || "";
  return `${value.slice(0, 11)}…${value.slice(-6)}`;
}
