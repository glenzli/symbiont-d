import { formatDuration, formatTokens, responseJson } from "/presentation.js";

// Owns the independently refreshed activity and usage projection. Settings owns
// configuration forms; this module owns the read-only operational snapshot.
export function initUsageUi(state) {
  const dialog = document.querySelector("#usage-dialog");
  const content = document.querySelector("#stats-content");
  const quotaState = document.querySelector("#quota-state");

  async function open() {
    content.textContent = "正在读取";
    dialog.showModal();
    try {
      const payload = await responseJson(await fetch("/api/stats"), "读取失败");
      state.usage = payload.headline;
      quotaState.textContent = `${quotaText(payload.rateLimits)} · 今日自主 ${formatTokens(payload.headline.autonomousTokensToday)} · 后台理解 ${formatTokens(payload.headline.reflectionTokensToday || 0)}`;
      render(payload.usage);
    } catch (error) {
      quotaState.textContent = "暂无法读取用量";
      content.textContent = error.message;
    }
  }

  function render(usage) {
    const totals = usage.totals;
    content.replaceChildren();

    const summary = document.createElement("div");
    summary.className = "stat-summary";
    for (const [label, value] of [
      ["调用", totals.invocations],
      ["总 token", formatTokens(totals.totalTokens)],
      ["推理 token", formatTokens(totals.reasoningOutputTokens)],
      ["累计耗时", formatDuration(totals.durationMs)],
    ]) {
      const item = document.createElement("div");
      const strong = document.createElement("strong");
      const span = document.createElement("span");
      strong.textContent = value;
      span.textContent = label;
      item.append(strong, span);
      summary.append(item);
    }
    content.append(summary);

    appendHeading(content, "按模型");
    const modelList = document.createElement("div");
    modelList.className = "model-usage-list";
    if (!usage.byModel.length) modelList.textContent = "还没有调用记录";
    for (const model of usage.byModel) {
      modelList.append(
        usageRow(
          model.displayName,
          `${model.invocations} 次 · ${formatTokens(model.totalTokens)} · ${formatDuration(model.durationMs)}`,
          "model-usage-row",
        ),
      );
    }
    content.append(modelList);

    appendHeading(content, "最近调用");
    const recentList = document.createElement("div");
    recentList.className = "recent-list";
    if (!usage.recent.length) recentList.textContent = "还没有调用记录";
    for (const invocation of usage.recent.slice(0, 12)) {
      const origin = {
        autonomous: "主动",
        reflection: "后台理解",
        maintenance: "记忆维护",
        interactive: "对话",
        continuation: "续话",
      }[invocation.origin] || invocation.origin;
      recentList.append(
        usageRow(
          `${invocation.modelDisplayName} · ${invocation.effort}`,
          `${origin} · ${invocation.lane} · ${formatTokens(invocation.totalTokens)} · ${formatDuration(invocation.durationMs)}`,
          "recent-row",
          invocation.id,
        ),
      );
    }
    content.append(recentList);
  }

  return { open };
}

function quotaText(rateLimits) {
  if (!rateLimits?.usedPercent && rateLimits?.usedPercent !== 0) {
    return "Codex 限额信息暂不可用";
  }
  const reset = rateLimits.resetsAt
    ? ` · ${new Date(rateLimits.resetsAt * 1000).toLocaleString()} 重置`
    : "";
  return `Codex 窗口已用 ${rateLimits.usedPercent.toFixed(0)}%${reset}`;
}

function appendHeading(parent, text) {
  const heading = document.createElement("h3");
  heading.textContent = text;
  parent.append(heading);
}

function usageRow(primaryText, secondaryText, className, traceId = null) {
  const row = document.createElement("div");
  row.className = className;
  const primary = document.createElement("span");
  const tail = document.createElement("span");
  tail.className = "usage-row-tail";
  const secondary = document.createElement("span");
  primary.className = "usage-primary";
  primary.textContent = primaryText;
  secondary.textContent = secondaryText;
  tail.append(secondary);
  if (traceId) {
    const trace = document.createElement("button");
    trace.type = "button";
    trace.className = "trace-button";
    trace.title = "查看执行轨迹";
    trace.setAttribute("aria-label", "查看执行轨迹");
    trace.dataset.traceId = traceId;
    trace.textContent = "⎇";
    tail.append(trace);
  }
  row.append(primary, tail);
  return row;
}
