import {
  formatTokens,
  millionsToTokens,
  responseJson,
  tokensToMillions,
} from "/presentation.js";
import { renderRichText } from "/rich-text.js";

export function initReflectionUi(state) {
  const form = document.querySelector("#reflection-form");
  const enabled = document.querySelector("#reflection-enabled");
  const settle = document.querySelector("#reflection-settle");
  const retention = document.querySelector("#reflection-retention");
  const tokenLimit = document.querySelector("#reflection-token-limit");
  const readState = document.querySelector("#reflection-read-state");
  const followUps = document.querySelector("#reflection-follow-ups");
  const continuations = document.querySelector("#reflection-continuations");
  const proactiveMessages = document.querySelector(
    "#reflection-proactive-messages",
  );
  const availability = document.querySelector("#reflection-availability");
  const runtimeState = document.querySelector("#reflection-runtime-state");
  const healthState = document.querySelector("#reflection-health-state");
  const saveState = document.querySelector("#reflection-save-state");
  const runButton = document.querySelector("#run-reflection");
  const archive = document.querySelector("#reflection-archive");
  const archiveTab = document.querySelector(
    '[data-archive-tab="reflection"]',
  );

  function renderConfig() {
    const config = state.reflection?.config;
    if (!config) return;
    enabled.checked = config.enabled;
    settle.value = String(config.settleSeconds);
    retention.value = String(config.retentionDays);
    tokenLimit.value = tokensToMillions(config.dailyTokenLimit || 0);
    readState.checked = config.captureReadState;
    followUps.checked = config.followUpsEnabled;
    continuations.checked = config.continuationsEnabled;
    proactiveMessages.checked = config.proactiveMessagesEnabled;
    availability.textContent = config.enabled ? "已启用" : "已关闭";
  }

  function renderRuntime() {
    const runtime = state.reflection?.runtime || state.reflection;
    if (!runtime) return;
    runtimeState.textContent = reflectionStatus(runtime);
    healthState.textContent = projectionHealth(state.reflection?.health);
    runButton.disabled = runtime.phase === "reflecting";
  }

  async function save(event) {
    event.preventDefault();
    saveState.textContent = "保存中";
    try {
      const config = await responseJson(
        await fetch("/api/reflection/config", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            enabled: enabled.checked,
            settleSeconds: Number(settle.value),
            retentionDays: Number(retention.value),
            captureReadState: readState.checked,
            followUpsEnabled: followUps.checked,
            continuationsEnabled: continuations.checked,
            proactiveMessagesEnabled: proactiveMessages.checked,
            dailyTokenLimit: millionsToTokens(tokenLimit.value),
          }),
        }),
        "保存失败",
      );
      state.reflection = { ...(state.reflection || {}), config };
      saveState.textContent = "已保存";
      renderConfig();
      renderRuntime();
    } catch (error) {
      saveState.textContent = error.message;
    }
  }

  async function run() {
    runButton.disabled = true;
    runtimeState.textContent = "正在加入队列";
    try {
      await responseJson(
        await fetch("/api/reflection/run", { method: "POST" }),
        "无法开始整理",
      );
    } catch (error) {
      runtimeState.textContent = error.message;
    } finally {
      window.setTimeout(renderRuntime, 1200);
    }
  }

  async function loadArchive() {
    archive.textContent = "正在读取";
    try {
      const snapshot = await responseJson(
        await fetch("/api/reflection"),
        "无法读取后台理解",
      );
      state.reflection = snapshot;
      renderConfig();
      renderRuntime();
      renderArchive(snapshot);
    } catch (error) {
      archive.textContent = error.message;
    }
  }

  function renderArchive(snapshot) {
    archive.replaceChildren();
    appendSection(
      archive,
      "Topic Episodes",
      snapshot.episodes,
      renderEpisode,
      "还没有形成值得长期整理的主题。",
    );
    appendSection(
      archive,
      "Working Hypotheses",
      snapshot.hypotheses,
      renderHypothesis,
      "当前没有工作假设。",
    );
    appendSection(
      archive,
      "Deferred Follow-ups",
      snapshot.followUps,
      renderFollowUp,
      "当前没有延迟跟进。",
    );
    appendSection(
      archive,
      "Recent Reflection",
      snapshot.recentRuns,
      renderRun,
      "还没有后台整理记录。",
    );
  }

  function renderEpisode(episode) {
    const item = document.createElement("article");
    item.className = "reflection-item";
    const header = itemHeader(
      episode.title,
      `${episodeState(episode.state)} · ${formatDate(episode.lastActivityAt)}`,
    );
    const body = document.createElement("div");
    body.className = "reflection-item-body";
    renderRichText(body, episode.summary);
    const graph = document.createElement("small");
    graph.className = "reflection-graph";
    const sources = episode.sourceRevisionIds?.length || 0;
    const parents = episode.parentEpisodeIds?.length || 0;
    graph.textContent = parents
      ? `${sources} 个来源 · 延续 ${parents} 个 Episode`
      : `${sources} 个来源 · 起点 Episode`;
    item.append(header, body, graph);
    return item;
  }

  function renderHypothesis(hypothesis) {
    const item = document.createElement("article");
    item.className = "reflection-item";
    if (hypothesis.horizon === "stable_candidate") {
      item.classList.add("profile-candidate");
    }
    const horizon =
      hypothesis.horizon === "stable_candidate"
        ? "画像候选"
        : hypothesis.horizon === "momentary"
          ? "即时"
          : "近期";
    const header = itemHeader(
      hypothesis.statement,
      `${horizon} · ${hypothesisState(hypothesis.status)} · ${formatDate(hypothesis.updatedAt)}`,
    );
    const facts = document.createElement("dl");
    facts.className = "reflection-evidence";
    appendDefinition(facts, "证据", hypothesis.evidence);
    appendDefinition(facts, "其他可能", hypothesis.alternatives);
    if (hypothesis.revisitAfter) {
      appendDefinition(facts, "再次检查", formatDate(hypothesis.revisitAfter));
    }
    item.append(header, facts);
    return item;
  }

  function renderFollowUp(followUp) {
    const item = document.createElement("article");
    item.className = "reflection-item";
    item.append(
      itemHeader(
        followUp.reason,
        `${followUpStatus(followUp.status)} · ${formatDate(followUp.notBefore)}`,
      ),
    );
    return item;
  }

  function renderRun(run) {
    const item = document.createElement("article");
    item.className = "reflection-run";
    const header = itemHeader(
      run.summary || reflectionRunStatus(run.status),
      `${formatDate(run.startedAt)} · ${run.eventCount} 个事件 · ${formatTokens(run.totalTokens || 0)}`,
    );
    if (run.traceId) {
      const trace = document.createElement("button");
      trace.type = "button";
      trace.className = "trace-button";
      trace.title = "查看执行轨迹";
      trace.setAttribute("aria-label", "查看执行轨迹");
      trace.dataset.traceId = run.traceId;
      trace.textContent = "⎇";
      header.append(trace);
    }
    if (run.actions?.length) {
      const actions = document.createElement("small");
      actions.className = "reflection-actions";
      actions.textContent = run.actions.join(" · ");
      item.append(header, actions);
    } else {
      item.append(header);
    }
    return item;
  }

  form.addEventListener("submit", save);
  runButton.addEventListener("click", run);
  archiveTab.addEventListener("click", loadArchive);

  return {
    render() {
      renderConfig();
      renderRuntime();
    },
    renderRuntime,
    loadArchive,
  };
}

function appendSection(parent, title, items, renderer, emptyText) {
  const section = document.createElement("section");
  section.className = "reflection-section";
  const heading = document.createElement("h3");
  heading.textContent = title;
  section.append(heading);
  if (!items?.length) {
    const empty = document.createElement("p");
    empty.className = "archive-empty";
    empty.textContent = emptyText;
    section.append(empty);
  } else {
    for (const item of items) section.append(renderer(item));
  }
  parent.append(section);
}

function itemHeader(titleText, metaText) {
  const header = document.createElement("header");
  const title = document.createElement("strong");
  const meta = document.createElement("small");
  title.textContent = titleText;
  meta.textContent = metaText;
  header.append(title, meta);
  return header;
}

function appendDefinition(parent, term, detail) {
  const dt = document.createElement("dt");
  const dd = document.createElement("dd");
  dt.textContent = term;
  dd.textContent = detail;
  parent.append(dt, dd);
}

function reflectionStatus(runtime) {
  if (runtime.phase === "reflecting") {
    return runtime.currentActivity || "正在整理近期对话";
  }
  if (runtime.phase === "disabled") return "已关闭";
  if (runtime.phase === "needs_setup") return "等待完成初始化";
  if (runtime.phase === "token_limit") return "今日分析预算已用尽";
  if (runtime.phase === "error") return runtime.lastError || "运行异常";
  if (runtime.pendingEvents) {
    const next = runtime.nextRunAt
      ? ` · ${new Date(runtime.nextRunAt).toLocaleTimeString([], {
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
        })}`
      : "";
    return `${runtime.pendingEvents} 个事件等待整理${next}`;
  }
  if (runtime.lastSummary) return runtime.lastSummary;
  return "等待新对话";
}

function episodeState(value) {
  return (
    {
      forming: "形成中",
      active: "活跃",
      dormant: "沉寂",
      closed: "结束",
    }[value] || value
  );
}

function hypothesisState(value) {
  return (
    {
      tentative: "暂定",
      working: "工作中",
      contradicted: "已反证",
      superseded: "已替代",
      stale: "待更新",
    }[value] || value
  );
}

function projectionHealth(health) {
  if (!health) return "等待数据健康检查";
  if (health.hypothesesMissingRevisit) {
    return `${health.hypothesesMissingRevisit} 条判断缺少复查时间`;
  }
  const due =
    (health.hypothesesDueForReview || 0) +
    (health.topicsDueForReview || 0);
  if (due) return `${due} 项已有必要重新检查`;
  return `${health.activeEpisodeCount || 0} 个活跃主题 · ${health.activeHypothesisCount || 0} 条有效判断`;
}

function followUpStatus(value) {
  return (
    {
      pending: "等待",
      triggered: "已触发",
      completed: "已完成",
      canceled: "已取消",
      cancelled: "已取消",
    }[value] || value
  );
}

function reflectionRunStatus(value) {
  return value === "error" ? "整理失败" : value === "running" ? "整理中" : "无变化";
}

function formatDate(value) {
  if (!value) return "未知时间";
  return new Date(value).toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
