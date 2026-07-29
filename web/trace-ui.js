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
      if (!response.ok) throw new Error(payload.error || "无法读取调用链");
      renderTrace(payload);
    } catch (error) {
      summary.textContent = "读取失败";
      content.textContent = error.message;
    }
  }

  function renderTrace(trace) {
    summary.textContent = `${trace.pcpToolCalls} 次 PCP 调用 · ${trace.runs.length} 个模型运行`;
    content.replaceChildren();
    for (const run of trace.runs) {
      const article = document.createElement("article");
      article.className = "trace-run";
      const header = document.createElement("header");
      const identity = document.createElement("span");
      const model = document.createElement("strong");
      const details = document.createElement("small");
      const id = document.createElement("code");
      model.textContent = run.displayName || run.model;
      details.textContent = `${run.lane} · ${run.effort} · ${formatTokens(
        run.totalTokens,
      )} · ${formatDuration(run.durationMs)}`;
      id.textContent = shortId(run.invocationId);
      identity.append(model, details);
      header.append(identity, id);
      article.append(header);

      const pcpSteps = run.steps.filter((step) => step.namespace === "pcp");
      for (const step of pcpSteps) article.append(renderTraceStep(step));
      content.append(article);
    }
    if (!trace.pcpToolCalls) {
      const empty = document.createElement("p");
      empty.className = "trace-empty";
      empty.textContent = "这次回复没有调用 PCP。";
      content.append(empty);
    }
  }
}

function renderTraceStep(step) {
  const details = document.createElement("details");
  details.className = "trace-step";
  details.dataset.success = String(step.succeeded);
  const summary = document.createElement("summary");
  const name = document.createElement("span");
  const timing = document.createElement("span");
  name.textContent = `${step.sequence + 1}. pcp.${step.tool}`;
  timing.textContent = `${step.succeeded ? "完成" : "失败"} · ${formatDuration(
    step.durationMs,
  )}`;
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

function tracePayload(label, value) {
  const fragment = document.createDocumentFragment();
  const heading = document.createElement("h4");
  const pre = document.createElement("pre");
  heading.textContent = label;
  pre.textContent = JSON.stringify(value, null, 2);
  fragment.append(heading, pre);
  return fragment;
}

function shortId(value) {
  if (!value || value.length < 20) return value || "";
  return `${value.slice(0, 11)}…${value.slice(-6)}`;
}
