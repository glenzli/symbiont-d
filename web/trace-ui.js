import { formatDuration, formatTokens } from "/presentation.js";
import { automaticRecallSummary, renderContextInspector } from "/context-inspector.js";

export function initTraceUi() {
  const dialog = document.querySelector("#trace-dialog");
  const summary = document.querySelector("#trace-summary");
  const content = document.querySelector("#trace-content");
  let requestGeneration = 0;
  dialog.addEventListener("close", () => { requestGeneration += 1; });

  document.addEventListener("click", (event) => {
    const button =
      event.target instanceof Element
        ? event.target.closest(".trace-button")
        : null;
    if (button?.dataset.traceId) openTrace(button.dataset.traceId);
  });

  async function openTrace(traceId) {
    const generation = ++requestGeneration;
    summary.textContent = "正在读取";
    content.textContent = "";
    dialog.showModal();
    try {
      const response = await fetch(
        `/api/traces/${encodeURIComponent(traceId)}`,
      );
      const payload = await response.json();
      if (generation !== requestGeneration) return;
      if (!response.ok) throw new Error(payload.error || "无法读取执行轨迹");
      renderTrace(payload);
    } catch (error) {
      if (generation !== requestGeneration) return;
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
    const sourceReads = (trace.runs || []).flatMap(run => run.steps || []).filter(step =>
      step.namespace === "symbiont" && step.tool === "resolve_source_ref" && step.succeeded).length;
    summary.textContent = `${trace.runs.length} 个模型运行 · ${trace.eventCount} 个可观察阶段 · ${automaticRecallSummary(trace)} · 模型追加 PCP 检索 ${executedRecallCount} 次${reuseSummary} · 原文溯源 ${sourceReads} 次 · ${writeCount} 次写入`;
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
      title.textContent = `模型追加 PCP 检索 ${executedRecallCount} 次`;
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
  const source = { luna: "Luna", external: "外部通道" }[run.inputSource];
  details.textContent = [
    activityLabel(run.activity),
    stageLabel(run.stage),
    source,
    run.lane,
    run.effort,
    formatTokens(run.totalTokens),
    formatDuration(run.durationMs),
  ]
    .filter(Boolean)
    .join(" · ");
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

  if (run.context) article.append(renderContextInspector(run.context));

  const timeline = document.createElement("section");
  timeline.className = "trace-timeline";
  const usedToolSteps = new Set();
  // Group by the host-issued proposal, never by similar text or tool name.
  const retentionGroups = new Map();
  for (const step of run.steps) {
    const proposalId = retentionReceipt(step)?.proposalId;
    if (typeof proposalId !== "string" || !proposalId) continue;
    if (!retentionGroups.has(proposalId)) retentionGroups.set(proposalId, []);
    retentionGroups.get(proposalId).push(step);
  }
  const renderedProposals = new Set();
  function appendStep(step, sequence) {
    const proposalId = retentionReceipt(step)?.proposalId;
    if (!retentionGroups.has(proposalId)) {
      timeline.append(renderTraceStep(step, sequence));
    } else if (!renderedProposals.has(proposalId)) {
      renderedProposals.add(proposalId);
      timeline.append(renderRetention(retentionGroups.get(proposalId), sequence));
    }
  }
  for (const event of run.events || []) {
    if (event.kind === "toolCall") {
      const sequence = event.details?.toolSequence;
      const step = run.steps.find((candidate) => candidate.sequence === sequence);
      if (step) {
        usedToolSteps.add(sequence);
        appendStep(step, event.sequence);
        continue;
      }
    }
    timeline.append(renderTraceEvent(event));
  }
  for (const step of run.steps) {
    if (!usedToolSteps.has(step.sequence)) {
      appendStep(step, null);
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

function renderRetention(steps, sequence) {
  const ordered = [...steps].sort((a, b) => a.sequence - b.sequence);
  const details = document.createElement("details");
  details.className = "trace-step trace-retention-process";
  const summary = document.createElement("summary");
  const name = document.createElement("span");
  const timing = document.createElement("span");
  const written = ordered.some(isPcpWrite);
  const last = ordered.at(-1);
  const state = written ? "已写入" : retentionPhase(last);
  name.textContent = `${sequence === null ? `工具 ${ordered[0].sequence + 1}` : sequence + 1}. PCP 留存 · ${state}`;
  timing.textContent = `${ordered.length} 个调用步骤`;
  summary.append(name, timing);
  const body = document.createElement("div");
  body.className = "trace-payload";
  const explanation = document.createElement("p");
  explanation.textContent = "预检不落库；模型复核后才可能写入。以下保留每次调用的输入、输出和原始顺序。";
  body.append(explanation, ...ordered.map(step => renderTraceStep(step, null)));
  details.dataset.pcpAction = written ? "write" : "retention";
  details.append(summary, body);
  return details;
}

function activityLabel(activity) {
  return {
    conversation: "对话",
    sensing: "感知",
    exploration: "主动探索",
    reflection: "对话整理",
    maintenance: "后台维护",
  }[activity] || "后台维护";
}

function stageLabel(stage) {
  return {
    reply: "回应",
    continuation: "续话",
    sense: "输入",
    review: "复核",
    scout: "深入探索",
    organize: "整理",
    context: "上下文",
    pcp: "PCP",
    reconciliation_preview: "预览整理",
    reconciliation_apply: "应用整理",
    internal: "内部运行",
  }[stage] || stage;
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
  const retention = step.namespace === "pcp" && step.tool === "write_page";
  label.textContent = `${position}. ${retention ? `PCP ${retentionPhase(step)}` : `${step.namespace}.${step.tool}`}`;
  if (retention) label.title = "pcp.write_page";
  name.append(label);
  if (isPcpRecall(step)) name.append(traceToolBadge("PCP 召回"));
  else if (isPcpWrite(step) && !retention) name.append(traceToolBadge("PCP 写入"));
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
    tracePayload("模型收到的输出", modelToolResult(step.result)),
  );
  if (step.result?._symbiontTrace) {
    payload.append(tracePayload("宿主诊断（未发送给模型）", step.result._symbiontTrace));
  }
  details.append(summary, payload);
  return details;
}

export function modelToolResult(result) {
  if (!result || typeof result !== "object" || Array.isArray(result)) return result;
  const { _symbiontTrace, ...model } = result;
  return model;
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
    ["browse_index", "search_pages", "semantic_search", "match_intent", "read_pages"].includes(step.tool)
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
  if (step.namespace === "pcp" && step.tool === "write_page") {
    if (!step.succeeded || step.result?._symbiontTrace?.deduplicated === true || deduplicatedFromSequence(step) !== null) return false;
    const receipt = retentionReceipt(step);
    // Same receipt-based rule as the backend. Unreadable evidence is not proof
    // of a write; only very old traces with no text receipt use legacy success.
    if (!receipt) return typeof step.result?.contentItems?.[0]?.text !== "string";
    return (!receipt.status || receipt.status === "written") && receipt.created !== false;
  }
  return (
    step.namespace === "pcp" &&
    [
      "assess_validity",
      "write_summary",
      "write_page",
      "revise_page",
      "consolidate_pages",
      "relate_pages",
    ].includes(step.tool)
  );
}

function retentionReceipt(step) {
  if (step.namespace !== "pcp" || step.tool !== "write_page") return null;
  try {
    const result = JSON.parse(step.result?.contentItems?.[0]?.text);
    return result && typeof result === "object" && !Array.isArray(result) ? result : null;
  } catch { return null; }
}

function retentionPhase(step) {
  if (!step.succeeded) return "留存调用失败";
  const receipt = retentionReceipt(step);
  if (deduplicatedFromSequence(step) !== null || receipt?.reusedReceipt) return "复用已有回执（未再次写入）";
  return {
    review_required: "写入预检（未写入）",
    written: receipt?.created === false ? "已有记录（未新建）" : "已写入",
    covered: "已有内容覆盖（未写入）",
    discarded: "仅保留本地聊天",
    deferred: "暂缓写入",
  }[receipt?.status] || "留存调用（结果未确认）";
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
