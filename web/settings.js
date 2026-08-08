import {
  formatDuration,
  formatTokens,
  millionsToTokens,
  responseJson,
  tokensToMillions,
} from "/presentation.js";
import { manualRunLabel, manualRunPending } from "/exploration-receipt.js";

export function initSettings(state, triggerExploration) {
  const dialog = document.querySelector("#settings-dialog");
  const computeForm = document.querySelector("#compute-form");
  const routingSelect = document.querySelector("#routing");
  const computeSaveState = document.querySelector("#compute-save-state");
  const ambientForm = document.querySelector("#ambient-form");
  const ambientSaveState = document.querySelector("#ambient-save-state");
  const ambientEmptyState = document.querySelector("#ambient-empty-state");
  const ambientProviderList = document.querySelector("#ambient-provider-list");
  const ambientProviderTemplate = document.querySelector("#ambient-provider-template");
  const addAmbientProvider = document.querySelector("#add-ambient-provider");
  const ambientChannelList = document.querySelector("#ambient-channel-list");
  const ambientChannelTemplate = document.querySelector("#ambient-channel-template");
  const addAmbientChannel = document.querySelector("#add-ambient-channel");
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

  function renderAmbient() {
    if (!state.ambient) return;
    ambientProviderList.replaceChildren();
    ambientChannelList.replaceChildren();
    for (const provider of state.ambient.providers || []) appendAmbientProvider(provider);
    for (const channel of state.ambient.channels || []) appendAmbientChannel(channel);
    ambientEmptyState.hidden = Boolean(
      state.ambient.providers?.length || state.ambient.channels?.length,
    );
  }

  function availabilityText(availability, runtime = {}) {
    if (runtime.lastError) return `上次失败：${runtime.lastError}`;
    if (availability === "ready") return "就绪";
    if (availability === "disabled") return "已关闭";
    if (availability === "missing_credential") return "未配置密钥";
    if (availability === "credential_unavailable") return "密钥暂不可读取";
    return availability || "尚不可用";
  }

  function credentialNote(provider) {
    if (provider.debugCredentialOverride) {
      return "调试构建：当前强制使用本地配置文件，不会读取系统凭据库。";
    }
    const location = provider.activeCredentialStore === "keychain"
      ? "系统凭据库"
      : "本地配置文件（权限 600）";
    if (provider.credentialStatus === "configured") {
      return `密钥已配置于${location}；不会显示或回传原文。`;
    }
    if (provider.credentialStatus === "unavailable") {
      return `${location}暂不可读取；后台探索不会弹出权限请求。`;
    }
    return "尚未配置密钥。保存时填写一次，密钥不会回显。";
  }

  function appendAmbientProvider(provider = {}, focus = false) {
    const fragment = ambientProviderTemplate.content.cloneNode(true);
    const row = fragment.querySelector("[data-ambient-provider]");
    row.querySelector('[data-provider-field="id"]').value = provider.id || "";
    row.querySelector('[data-provider-field="baseUrl"]').value = provider.baseUrl || "";
    row.querySelector('[data-provider-field="credentialStore"]').value =
      provider.credentialStore || "config_file";
    row.querySelector('[data-provider-field="credentialValue"]').value = "";
    row.querySelector('[data-provider-field="webSearchTool"]').value = provider.webSearchTool || "web_search";
    row.querySelector('[data-provider-field="enabled"]').checked = provider.enabled === true;
    row.querySelector(".ambient-row-title").textContent = provider.id || "新 Provider";
    row.querySelector(".ambient-row-status").textContent = availabilityText(provider.availability);
    row.querySelector(".ambient-credential-note").textContent = credentialNote(provider);
    ambientProviderList.append(fragment);
    if (focus) row.querySelector('[data-provider-field="id"]').focus();
  }

  function appendAmbientChannel(channel = {}, focus = false) {
    const fragment = ambientChannelTemplate.content.cloneNode(true);
    const row = fragment.querySelector("[data-ambient-channel]");
    const provider = row.querySelector('[data-channel-field="providerId"]');
    const providers = [
      ...ambientProviderList.querySelectorAll("[data-ambient-provider]"),
    ].map((row) => ({
      id: row.querySelector('[data-provider-field="id"]').value.trim(),
    })).filter((entry) => entry.id);
    provider.replaceChildren(
      ...providers.map((entry) => {
        const option = document.createElement("option");
        option.value = entry.id;
        option.textContent = entry.id;
        return option;
      }),
    );
    row.querySelector('[data-channel-field="id"]').value = channel.id || "";
    provider.value = channel.providerId || provider.value;
    row.querySelector('[data-channel-field="name"]').value = channel.name || "";
    row.querySelector('[data-channel-field="model"]').value = channel.model || "";
    row.querySelector('[data-channel-field="focus"]').value = channel.focus || "";
    row.querySelector('[data-channel-field="intervalMinutes"]').value = String(channel.intervalMinutes || 180);
    row.querySelector('[data-channel-field="enabled"]').checked = channel.enabled !== false;
    row.querySelector(".ambient-row-title").textContent = channel.name || channel.id || "新输入通道";
    row.querySelector(".ambient-row-status").textContent = availabilityText(channel.availability, channel);
    ambientChannelList.append(fragment);
    if (focus) row.querySelector('[data-channel-field="id"]').focus();
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
    dailyNoteLimit.value = String(state.autonomy.dailyNoteLimit ?? 4);
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
      !state.autonomyPermitted || manualRunPending(state.exploration);
  }

  function renderAutonomy() {
    renderAutonomyConfig();
    renderAutonomyRuntime();
  }

  function renderBridge() {
    codexTaskAccess.checked = state.bridge?.codexTaskAccess === true;
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

  function ambientFormValue() {
    return {
      providers: [...ambientProviderList.querySelectorAll("[data-ambient-provider]")].map((row) => ({
        id: row.querySelector('[data-provider-field="id"]').value.trim(),
        enabled: row.querySelector('[data-provider-field="enabled"]').checked,
        baseUrl: row.querySelector('[data-provider-field="baseUrl"]').value.trim(),
        credentialStore: row.querySelector('[data-provider-field="credentialStore"]').value,
        credentialValue:
          row.querySelector('[data-provider-field="credentialValue"]').value || null,
        webSearchTool: row.querySelector('[data-provider-field="webSearchTool"]').value.trim(),
      })),
      channels: [...ambientChannelList.querySelectorAll("[data-ambient-channel]")].map((row) => ({
        id: row.querySelector('[data-channel-field="id"]').value.trim(),
        enabled: row.querySelector('[data-channel-field="enabled"]').checked,
        providerId: row.querySelector('[data-channel-field="providerId"]').value,
        name: row.querySelector('[data-channel-field="name"]').value.trim(),
        model: row.querySelector('[data-channel-field="model"]').value.trim(),
        focus: row.querySelector('[data-channel-field="focus"]').value.trim(),
        intervalMinutes: Number(row.querySelector('[data-channel-field="intervalMinutes"]').value),
      })),
    };
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

  async function saveAmbient(event) {
    event.preventDefault();
    ambientSaveState.textContent = "保存中";
    try {
      state.ambient = await responseJson(
        await fetch("/api/ambient", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(ambientFormValue()),
        }),
        "信息入口保存失败",
      );
      renderAmbient();
      ambientSaveState.textContent = "已保存";
    } catch (error) {
      ambientSaveState.textContent = error.message;
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
    renderAmbient();
    renderAutonomy();
    renderBridge();
    computeSaveState.textContent = "";
    ambientSaveState.textContent = "";
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
  ambientForm.addEventListener("submit", saveAmbient);
  addComputePolicy.addEventListener("click", () => appendComputePolicy({}, true));
  computePolicyList.addEventListener("click", (event) => {
    const button = event.target.closest(".remove-compute-policy");
    if (button) button.closest(".compute-policy-row").remove();
  });
  addAmbientProvider.addEventListener("click", () =>
    appendAmbientProvider(
      {
        id: "new-provider",
        enabled: false,
        baseUrl: "https://api.openai.com/v1",
        credentialStore: "config_file",
        webSearchTool: "web_search",
      },
      true,
    ),
  );
  addAmbientChannel.addEventListener("click", () => {
    const providerId = ambientProviderList.querySelector(
      '[data-provider-field="id"]',
    )?.value.trim();
    if (!providerId) {
      ambientSaveState.textContent = "请先添加一个 Provider";
      addAmbientProvider.focus();
      return;
    }
    appendAmbientChannel(
      {
        id: "new-channel",
        enabled: true,
        providerId,
        name: "新的广域观察",
        model: "gpt-5-mini",
        focus: "Describe the independent external domain this role should observe.",
        intervalMinutes: 180,
      },
      true,
    );
  });
  ambientProviderList.addEventListener("click", (event) => {
    const button = event.target.closest(".remove-ambient-provider");
    if (button) button.closest("[data-ambient-provider]").remove();
  });
  ambientChannelList.addEventListener("click", (event) => {
    const button = event.target.closest(".remove-ambient-channel");
    if (button) button.closest("[data-ambient-channel]").remove();
  });
  autonomyForm.addEventListener("submit", saveAutonomy);
  bridgeForm.addEventListener("submit", saveBridge);
  codexTaskAccess.addEventListener("change", () => {
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
      renderAmbient();
      renderAutonomy();
      renderBridge();
    },
    renderAutonomy,
    renderAutonomyRuntime,
    renderAmbient,
    open: openSettings,
  };
}

function explorationStatusText(exploration, usage, autonomy) {
  if (!exploration) return "正在读取";
  if (manualRunPending(exploration)) {
    return manualRunLabel(exploration.manualRun);
  }
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
    return `今日主动消息额度已用尽 · 介入 ${usage?.autonomousInterventionsToday || 0}/${autonomy?.dailyInterruptLimit || 0} · 留话 ${usage?.autonomousNotesToday || 0}/${autonomy?.dailyNoteLimit ?? 4}`;
  }
  if (phase === "error") return exploration.lastError || "探索运行出错";
  if (exploration.lastOutcome === "channel_failed") {
    return "某个信息入口本轮失效；可在信息入口设置中查看";
  }
  const candidates = exploration.pendingCandidateCount
    ? `最近候选 ${exploration.pendingCandidateCount} 条 · `
    : "";
  return exploration.nextRunAt
    ? `${candidates}下次感知 ${formatNext(exploration.nextRunAt)}`
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
