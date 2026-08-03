import {
  formatDuration,
  formatTokens,
  millionsToTokens,
  responseJson,
  tokensToMillions,
} from "/presentation.js";

export function initSettings(state, triggerExploration) {
  const dialog = document.querySelector("#settings-dialog");
  const computeForm = document.querySelector("#compute-form");
  const routingSelect = document.querySelector("#routing");
  const computeSaveState = document.querySelector("#compute-save-state");
  const computePolicyList = document.querySelector("#compute-policy-list");
  const computePolicyTemplate = document.querySelector(
    "#compute-policy-template",
  );
  const addComputePolicy = document.querySelector("#add-compute-policy");
  const autonomyForm = document.querySelector("#autonomy-form");
  const autonomyEnabled = document.querySelector("#autonomy-enabled");
  const autonomyInterval = document.querySelector("#autonomy-interval");
  const dailyInterruptLimit = document.querySelector("#daily-interrupt-limit");
  const dailyNoteLimit = document.querySelector("#daily-note-limit");
  const dailyTokenLimit = document.querySelector("#daily-token-limit");
  const attentionPosture = document.querySelector("#attention-posture");
  const quietHoursEnabled = document.querySelector("#quiet-hours-enabled");
  const quietHoursStart = document.querySelector("#quiet-hours-start");
  const quietHoursEnd = document.querySelector("#quiet-hours-end");
  const autonomyAvailability = document.querySelector("#autonomy-availability");
  const autonomyRuntimeState = document.querySelector("#autonomy-runtime-state");
  const runExploration = document.querySelector("#run-exploration");
  const autonomySaveState = document.querySelector("#autonomy-save-state");
  const statsContent = document.querySelector("#stats-content");
  const quotaState = document.querySelector("#quota-state");
  const bridgeForm = document.querySelector("#bridge-form");
  const codexTaskAccess = document.querySelector("#codex-task-access");
  const codexProjectHandoffs = document.querySelector("#codex-task-execution");
  const codexProjectHandoffsNote = document.querySelector(
    "#codex-task-execution-note",
  );
  const bridgeSaveState = document.querySelector("#bridge-save-state");
  const tabButtons = [...dialog.querySelectorAll("[data-settings-tab]")];
  const tabPanels = [...dialog.querySelectorAll("[data-settings-panel]")];

  function modelBySlug(slug) {
    return state.models.find(
      (model) => model.model === slug || model.id === slug,
    );
  }

  function configureEffortSelect(row, selectedEffort) {
    const modelSelect = row.querySelector('[data-field="model"]');
    const effortSelect = row.querySelector('[data-field="effort"]');
    const model = modelBySlug(modelSelect.value);
    const efforts = model?.supportedReasoningEfforts || [];
    effortSelect.replaceChildren(
      ...efforts.map((effort) => {
        const option = document.createElement("option");
        option.value = effort.reasoningEffort;
        option.textContent = effort.reasoningEffort;
        option.title = effort.description;
        return option;
      }),
    );
    effortSelect.value = efforts.some(
      (effort) => effort.reasoningEffort === selectedEffort,
    )
      ? selectedEffort
      : model?.defaultReasoningEffort || "";
  }

  function renderCompute() {
    if (!state.compute) return;
    routingSelect.value = state.compute.routing;
    for (const row of computeForm.querySelectorAll(".lane-row")) {
      const lane = row.dataset.lane;
      const modelSelect = row.querySelector('[data-field="model"]');
      modelSelect.replaceChildren(
        ...state.models.map((model) => {
          const option = document.createElement("option");
          option.value = model.model;
          option.textContent = model.displayName;
          option.title = model.description;
          return option;
        }),
      );
      modelSelect.value = state.compute.lanes[lane].model;
      configureEffortSelect(row, state.compute.lanes[lane].effort);
    }
    computePolicyList.replaceChildren();
    for (const policy of state.computePolicies || []) {
      appendComputePolicy(policy);
    }
  }

  function appendComputePolicy(policy = {}, focus = false) {
    const fragment = computePolicyTemplate.content.cloneNode(true);
    const row = fragment.querySelector(".compute-policy-row");
    row.querySelector('[data-policy-field="id"]').value = policy.id || "";
    row.querySelector('[data-policy-field="topic"]').value = policy.topic || "";
    row.querySelector('[data-policy-field="aliases"]').value = (
      policy.aliases || []
    ).join(", ");
    row.querySelector('[data-policy-field="minimumLane"]').value =
      policy.minimumLane || "critical";
    row.querySelector('[data-policy-field="enabled"]').checked =
      policy.enabled !== false;
    computePolicyList.append(fragment);
    if (focus) row.querySelector('[data-policy-field="topic"]').focus();
  }

  function renderAutonomyConfig() {
    if (!state.autonomy) return;
    autonomyEnabled.checked = state.autonomy.enabled;
    autonomyInterval.value = String(state.autonomy.intervalMinutes);
    dailyInterruptLimit.value = String(state.autonomy.dailyInterruptLimit);
    dailyNoteLimit.value = String(state.autonomy.dailyNoteLimit ?? 2);
    dailyTokenLimit.value = tokensToMillions(
      state.autonomy.dailyTokenLimit || 0,
    );
    attentionPosture.value = state.autonomy.attentionPosture || "";
    quietHoursEnabled.checked = state.autonomy.quietHours.enabled;
    quietHoursStart.value = state.autonomy.quietHours.start;
    quietHoursEnd.value = state.autonomy.quietHours.end;
    toggleQuietInputs();
  }

  function renderAutonomyRuntime() {
    if (!state.autonomy) return;
    if (state.profile.status !== "ready") {
      autonomyAvailability.textContent = "初始化完成后生效";
    } else if (state.autonomyPermitted) {
      autonomyAvailability.textContent = "已启用";
    } else {
      autonomyAvailability.textContent = "当前关闭";
    }
    autonomyRuntimeState.textContent = explorationStatusText(
      state.exploration,
      state.usage,
      state.autonomy,
    );
    runExploration.disabled =
      !state.autonomyPermitted || state.exploration?.phase === "exploring";
  }

  function renderAutonomy() {
    renderAutonomyConfig();
    renderAutonomyRuntime();
  }

  function renderBridge() {
    codexTaskAccess.checked = state.bridge?.codexTaskAccess === true;
    const lease = state.bridge?.activeProjectLease;
    const project = lease?.project || state.bridge?.selectedProject;
    codexProjectHandoffs.checked =
      state.bridge?.projectHandoffsEnabled === true;
    codexProjectHandoffs.disabled = !codexTaskAccess.checked;
    codexProjectHandoffsNote.textContent = project
      ? `只对选定项目生效 · 当前 ${project.title} · ${shortPath(project.cwd)}`
      : "只在输入区明确选择项目后生效";
  }

  function toggleQuietInputs() {
    quietHoursStart.disabled = !quietHoursEnabled.checked;
    quietHoursEnd.disabled = !quietHoursEnabled.checked;
  }

  function computeFormValue() {
    const lanes = {};
    for (const row of computeForm.querySelectorAll(".lane-row")) {
      lanes[row.dataset.lane] = {
        model: row.querySelector('[data-field="model"]').value,
        effort: row.querySelector('[data-field="effort"]').value,
      };
    }
    return {
      routing: routingSelect.value,
      showModel: true,
      lanes,
    };
  }

  function computePolicyFormValue() {
    return [...computePolicyList.querySelectorAll(".compute-policy-row")]
      .map((row) => ({
        id: row.querySelector('[data-policy-field="id"]').value || null,
        topic: row.querySelector('[data-policy-field="topic"]').value.trim(),
        aliases: row
          .querySelector('[data-policy-field="aliases"]')
          .value.split(/[,，\n]/)
          .map((alias) => alias.trim())
          .filter(Boolean),
        minimumLane: row.querySelector('[data-policy-field="minimumLane"]')
          .value,
        enabled: row.querySelector('[data-policy-field="enabled"]').checked,
      }))
      .filter((policy) => policy.topic);
  }

  async function saveCompute(event) {
    event.preventDefault();
    computeSaveState.textContent = "保存中";
    try {
      state.compute = await responseJson(
        await fetch("/api/compute", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(computeFormValue()),
        }),
        "保存失败",
      );
      state.computePolicies = await responseJson(
        await fetch("/api/compute/policies", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(computePolicyFormValue()),
        }),
        "话题规则保存失败",
      );
      renderCompute();
      computeSaveState.textContent = "已保存";
    } catch (error) {
      computeSaveState.textContent = error.message;
    }
  }

  async function saveAutonomy(event) {
    event.preventDefault();
    autonomySaveState.textContent = "保存中";
    const config = {
      enabled: autonomyEnabled.checked,
      intervalMinutes: Number(autonomyInterval.value),
      dailyInterruptLimit: Number(dailyInterruptLimit.value),
      dailyNoteLimit: Number(dailyNoteLimit.value),
      dailyTokenLimit: millionsToTokens(dailyTokenLimit.value),
      attentionPosture: attentionPosture.value.trim(),
      quietHours: {
        enabled: quietHoursEnabled.checked,
        start: quietHoursStart.value,
        end: quietHoursEnd.value,
      },
    };
    try {
      state.autonomy = await responseJson(
        await fetch("/api/autonomy", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(config),
        }),
        "保存失败",
      );
      state.autonomyPermitted =
        state.profile.status === "ready" && state.autonomy.enabled;
      autonomySaveState.textContent = "已保存";
      renderAutonomy();
    } catch (error) {
      autonomySaveState.textContent = error.message;
    }
  }

  async function saveBridge(event) {
    event.preventDefault();
    bridgeSaveState.textContent = "保存中";
    try {
      state.bridge = await responseJson(
        await fetch("/api/bridge/config", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            codexTaskAccess: codexTaskAccess.checked,
            projectHandoffsEnabled:
              codexTaskAccess.checked && codexProjectHandoffs.checked,
          }),
        }),
        "保存失败",
      );
      bridgeSaveState.textContent = "已保存";
      renderBridge();
    } catch (error) {
      bridgeSaveState.textContent = error.message;
    }
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

  async function loadStats() {
    statsContent.textContent = "正在读取";
    try {
      const payload = await responseJson(
        await fetch("/api/stats"),
        "读取失败",
      );
      state.usage = payload.headline;
      quotaState.textContent = `${quotaText(payload.rateLimits)} · 今日自主 ${formatTokens(payload.headline.autonomousTokensToday)} · 后台理解 ${formatTokens(payload.headline.reflectionTokensToday || 0)}`;
      renderStats(payload.usage);
    } catch (error) {
      statsContent.textContent = error.message;
    }
  }

  function renderStats(usage) {
    const totals = usage.totals;
    statsContent.replaceChildren();

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
    statsContent.append(summary);

    appendHeading(statsContent, "按模型");
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
    statsContent.append(modelList);

    appendHeading(statsContent, "最近调用");
    const recentList = document.createElement("div");
    recentList.className = "recent-list";
    if (!usage.recent.length) recentList.textContent = "还没有调用记录";
    for (const invocation of usage.recent.slice(0, 12)) {
      const origin =
        {
          autonomous: "主动",
          reflection: "后台理解",
          maintenance: "记忆维护",
          interactive: "对话",
          codex_handoff: "Codex 交接",
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
    statsContent.append(recentList);
  }

  function activateTab(name) {
    for (const button of tabButtons) {
      button.setAttribute(
        "aria-selected",
        String(button.dataset.settingsTab === name),
      );
    }
    for (const panel of tabPanels) {
      panel.hidden = panel.dataset.settingsPanel !== name;
    }
    if (name === "stats") loadStats();
  }

  async function runManualExploration() {
    runExploration.disabled = true;
    autonomyRuntimeState.textContent = "正在加入探索队列";
    try {
      const result = await triggerExploration();
      autonomyRuntimeState.textContent = result.canceled
        ? "已取消"
        : result.alreadyQueued
          ? "探索已在进行或排队"
          : "即将开始";
    } catch (error) {
      autonomyRuntimeState.textContent = error.message;
    } finally {
      window.setTimeout(renderAutonomyRuntime, 1500);
    }
  }

  function openSettings(tab = "autonomy") {
    renderCompute();
    renderAutonomy();
    renderBridge();
    computeSaveState.textContent = "";
    autonomySaveState.textContent = "";
    activateTab(tab);
    dialog.showModal();
  }

  computeForm.addEventListener("change", (event) => {
    const row = event.target.closest(".lane-row");
    if (row && event.target.matches('[data-field="model"]')) {
      configureEffortSelect(row);
    }
  });
  computeForm.addEventListener("submit", saveCompute);
  addComputePolicy.addEventListener("click", () => appendComputePolicy({}, true));
  computePolicyList.addEventListener("click", (event) => {
    const button = event.target.closest(".remove-compute-policy");
    if (button) button.closest(".compute-policy-row").remove();
  });
  autonomyForm.addEventListener("submit", saveAutonomy);
  bridgeForm.addEventListener("submit", saveBridge);
  codexTaskAccess.addEventListener("change", () => {
    if (!codexTaskAccess.checked) codexProjectHandoffs.checked = false;
    codexProjectHandoffs.disabled = !codexTaskAccess.checked;
  });
  runExploration.addEventListener("click", runManualExploration);
  quietHoursEnabled.addEventListener("change", toggleQuietInputs);
  for (const button of tabButtons) {
    button.addEventListener("click", () =>
      activateTab(button.dataset.settingsTab),
    );
  }
  document.querySelector("#open-settings").addEventListener("click", () => {
    openSettings("autonomy");
  });

  return {
    render() {
      renderCompute();
      renderAutonomy();
      renderBridge();
    },
    renderAutonomy,
    renderAutonomyRuntime,
    open: openSettings,
  };
}

function shortPath(path) {
  if (!path) return "";
  return path.split("/").filter(Boolean).slice(-2).join("/");
}

function explorationStatusText(exploration, usage, autonomy) {
  if (!exploration) return "正在读取";
  const phase = exploration.phase;
  if (phase === "exploring") {
    return exploration.currentActivity?.label || "正在探索";
  }
  if (phase === "disabled") return "已关闭";
  if (phase === "needs_setup") return "等待完成初始化";
  if (phase === "quiet_hours") {
    return `安静时段 · ${formatNext(exploration.nextRunAt)}`;
  }
  if (phase === "token_limit") {
    return `今日预算已用尽 · ${formatTokens(usage?.autonomousTokensToday || 0)}`;
  }
  if (phase === "message_limit") {
    return `今日主动消息额度已用尽 · 介入 ${usage?.autonomousInterventionsToday || 0}/${autonomy?.dailyInterruptLimit || 0} · 留话 ${usage?.autonomousNotesToday || 0}/${autonomy?.dailyNoteLimit ?? 2}`;
  }
  if (phase === "error") return exploration.lastError || "探索运行出错";
  return exploration.nextRunAt
    ? `下次 ${formatNext(exploration.nextRunAt)}`
    : "等待调度";
}

function formatNext(value) {
  if (!value) return "稍后";
  return new Date(value).toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
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
