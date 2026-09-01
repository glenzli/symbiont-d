// This projection is diagnostic only. Nothing rendered here is sent to a model.
const labels = {
  "symbiont.time": "当前时间",
  "symbiont.compute": "计算边界",
  "symbiont.profile": "稳定身份与偏好",
  "symbiont.memory_boundary": "记忆身份与权限",
  "symbiont.route": "本轮路由",
  "symbiont.bridge": "跨入口边界",
  "symbiont.recall_status": "自动召回状态",
  "symbiont.working_context": "最近对话桥接",
  "symbiont.interaction": "输出约定",
  "symbiont.rollover": "线程轮换",
  "symbiont.background.map": "工作地图与开放问题",
  "symbiont.background.curiosity": "探索问题与反馈",
  "symbiont.background.reflection": "互动事件、主题与假说",
  "symbiont.background.compute_policies": "计算规则库",
  "symbiont.pcp": "旧版混合上下文（不等于 PCP 召回）",
};
const chars = value => Array.from(typeof value === "string" ? value : JSON.stringify(value ?? null)).length;
const count = value => Number(value || 0).toLocaleString("zh-CN");

export function submittedContextExport(context) {
  if (!context.submitted) return null;
  return JSON.stringify({
    boundary: "Exact client-submitted thread/start and turn/start values at this turn's start. Not the provider's final prompt. Later tool replies appear in the execution trace. Images may be local-path references, not embedded image bytes.",
    ...context.submitted,
    observableNativeHistory: context.nativeThread,
    diagnosticSelectionNotSentToModel: context.selection || [],
  }, null, 2);
}

function payload(doc, label, value) {
  const details = doc.createElement("details");
  details.className = "trace-raw";
  const summary = doc.createElement("summary");
  summary.textContent = label;
  details.append(summary);
  details.addEventListener("toggle", () => {
    if (!details.open || details.querySelector("pre")) return;
    const pre = doc.createElement("pre");
    pre.textContent = typeof value === "string" ? value : JSON.stringify(value, null, 2);
    details.append(pre);
  });
  return details;
}

export function renderContextInspector(context, doc = document) {
  const details = doc.createElement("details");
  details.className = "trace-context";
  const summary = doc.createElement("summary");
  const title = doc.createElement("span");
  const state = doc.createElement("span");
  const native = context.nativeThread || {};
  const fragments = context.fragments || [];
  title.textContent = "输入上下文 · 来源与完整提交";
  state.textContent = `${count(native.priorTurns)} 个既有 turn · ${count(native.compactionsBefore)} 次压缩`;
  summary.append(title, state);
  const body = doc.createElement("div");
  body.className = "trace-context-body";
  const notice = doc.createElement("p");
  notice.className = "trace-context-notice";
  notice.textContent = "这里展示本轮起点的客户端提交。底层系统提示、完整原生历史、内部压缩及最终 token 序列未暴露，不能称为模型的完整最终提示词。后续工具返回请看下方执行轨迹。";
  body.append(notice);

  const stats = doc.createElement("p");
  stats.className = "trace-context-notice";
  const textChars = (context.input || []).reduce((sum, item) => sum + chars(item.text || ""), 0);
  const fragmentChars = fragments.reduce((sum, part) => sum + chars(part.value), 0);
  const tools = context.submitted?.threadStart?.dynamicTools;
  stats.textContent = `直接输入文字 ${count(textChars)} 字符 · 应用上下文 ${count(fragmentChars)} 字符 · 线程指令 ${count(chars(context.developerInstructions || ""))} 字符${tools ? ` · 工具定义 JSON ${count(chars(tools))} 字符` : " · 工具定义未记录"}。字符数不等于 tokens。`;
  body.append(stats);
  const nativeInfo = doc.createElement("p");
  nativeInfo.className = "trace-context-notice";
  nativeInfo.textContent = `线程 ${native.threadId || "未记录"} · 窗口容量 ${native.modelContextWindow ? count(native.modelContextWindow) + " tokens（不是实际输入量）" : "未报告"} · 最近桥接 ${context.workingContext?.messages?.length || 0} 条${context.workingContext?.truncated ? "（较早部分省略，可检索）" : ""}`;
  body.append(nativeInfo);

  const rows = context.selection || [];
  body.append(payload(doc, "本轮直接输入 · 用户消息／当前任务包", context.input));
  for (const part of fragments) {
    const provenance = rows.find(row => row.source === part.source && row.included);
    const name = labels[part.source] || (part.source.startsWith("symbiont.transcript.") ? "本地聊天原文" : part.source.startsWith("symbiont.pcp.") ? "PCP 长期记忆" : part.source);
    const section = doc.createElement("section");
    section.className = "context-source";
    section.dataset.source = part.source;
    const info = doc.createElement("p");
    info.textContent = provenance ? `${provenance.origin} · ${provenance.purpose}` : "旧轨迹未记录细分来源；以下为当时保存的实际片段。";
    const raw = payload(doc, `${name} · ${count(chars(part.value))} 字符`, part.value);
    const source = doc.createElement("code");
    source.textContent = part.source;
    section.append(raw, source, info);
    body.append(section);
  }
  const omitted = rows.filter(row => !row.included);
  if (omitted.length) {
    const excluded = doc.createElement("details");
    excluded.className = "trace-raw context-deferred";
    const heading = doc.createElement("summary");
    heading.textContent = `本轮未装入 · ${omitted.length} 项（以下原因也未发送给模型）`;
    const list = doc.createElement("ul");
    for (const row of omitted) {
      const item = doc.createElement("li");
      item.textContent = `${labels[row.source] || row.source} — ${row.origin}：${row.purpose}`;
      list.append(item);
    }
    excluded.append(heading, list);
    body.append(excluded);
  }
  body.append(payload(doc, "线程 developer instructions · 实际注册的指令", context.developerInstructions));
  if (tools) body.append(payload(doc, "线程工具定义 · 实际注册的工具", tools));
  if (native.observableHistoryTail?.length) {
    body.append(payload(doc, `原生线程可观察历史${native.historyTailTruncated ? "尾部（更早部分未提供）" : "（不保证等于模型当前工作历史）"}`, native.observableHistoryTail));
  }
  if (context.workingContext) body.append(payload(doc, "桥接诊断 manifest（不额外重复发送）", context.workingContext));

  const exported = submittedContextExport(context);
  if (exported !== null) {
    body.append(payload(doc, "完整客户端提交 · 线程配置＋本轮请求＋可观察历史", exported));
    const actions = doc.createElement("div");
    actions.className = "context-export-actions";
    const status = doc.createElement("span");
    status.setAttribute("role", "status");
    const copy = doc.createElement("button");
    copy.type = "button";
    copy.textContent = "复制完整提交";
    copy.addEventListener("click", async () => {
      try {
        await doc.defaultView.navigator.clipboard.writeText(exported);
        status.textContent = "已复制";
      } catch {
        status.textContent = "剪贴板不可用，请展开查看或下载 JSON";
      }
    });
    const download = doc.createElement("button");
    download.type = "button";
    download.textContent = "下载 JSON";
    download.addEventListener("click", () => {
      try {
        const win = doc.defaultView;
        const url = win.URL.createObjectURL(new win.Blob([exported], { type: "application/json" }));
        const link = doc.createElement("a");
        link.href = url;
        const suffix = String(context.submitted.turnStart?.threadId || "run").replace(/[^a-zA-Z0-9_-]/g, "");
        link.download = `symbiont-context-${suffix}.json`;
        link.click();
        win.setTimeout(() => win.URL.revokeObjectURL(url), 1000);
      } catch {
        status.textContent = "当前窗口不支持下载，请复制或展开查看";
      }
    });
    actions.append(copy, download, status);
    body.append(actions);
  } else {
    const legacy = doc.createElement("p");
    legacy.className = "trace-context-notice";
    legacy.textContent = "旧轨迹没有保存完整请求与工具注册，不能事后还原；上面仍可查看当时记录的各部分。";
    body.append(legacy);
  }
  details.append(summary, body);
  return details;
}
