import {
  millionsToTokens,
  responseJson,
  tokensToMillions,
} from "/presentation.js";

export function initSettings(state) {
  const dialog = document.querySelector("#settings-dialog");
  const computeForm = document.querySelector("#compute-form");
  const routingSelect = document.querySelector("#routing");
  const computeSaveState = document.querySelector("#compute-save-state");
  const ambientForm = document.querySelector("#ambient-form");
  const ambientSaveState = document.querySelector("#ambient-save-state");
  const ambientEmptyState = document.querySelector("#ambient-empty-state");
  const lunaEnabled = document.querySelector("#luna-enabled");
  const lunaFocus = document.querySelector("#luna-focus");
  const lunaAvailability = document.querySelector("#luna-availability");
  const ambientProviderList = document.querySelector("#ambient-provider-list");
  const ambientProviderTemplate = document.querySelector("#ambient-provider-template");
  const addAmbientProvider = document.querySelector("#add-ambient-provider");
  const ambientChannelList = document.querySelector("#ambient-channel-list");
  const ambientChannelTemplate = document.querySelector("#ambient-channel-template");
  const addAmbientChannel = document.querySelector("#add-ambient-channel");
  const mailInputForm = document.querySelector("#mail-input-form");
  const mailInputName = document.querySelector("#mail-input-name");
  const mailInputNameLabel = document.querySelector("#mail-input-name-label");
  const mailInputHost = document.querySelector("#mail-input-host");
  const mailInputPort = document.querySelector("#mail-input-port");
  const mailInputUsername = document.querySelector("#mail-input-username");
  const mailInputFolder = document.querySelector("#mail-input-folder");
  const mailInputMaxMessages = document.querySelector("#mail-input-max-messages");
  const mailInputCredentialStore = document.querySelector("#mail-input-credential-store");
  const mailInputCredentialValue = document.querySelector("#mail-input-credential-value");
  const mailInputAllowedSenders = document.querySelector("#mail-input-allowed-senders");
  const mailInputEnabled = document.querySelector("#mail-input-enabled");
  const mailInputAvailability = document.querySelector("#mail-input-availability");
  const mailInputCredentialNote = document.querySelector("#mail-input-credential-note");
  const mailInputRuntimeNote = document.querySelector("#mail-input-runtime-note");
  const mailInputSaveState = document.querySelector("#mail-input-save-state");
  const computePolicyList = document.querySelector("#compute-policy-list");
  const computePolicyTemplate = document.querySelector(
    "#compute-policy-template",
  );
  const addComputePolicy = document.querySelector("#add-compute-policy");
  const autonomyForm = document.querySelector("#autonomy-form");
  const autonomyEnabled = document.querySelector("#autonomy-enabled");
  const autonomyInterval = document.querySelector("#autonomy-interval");
  const maxInputParallelism = document.querySelector("#max-input-parallelism");
  const dailyInterruptLimit = document.querySelector("#daily-interrupt-limit");
  const dailyNoteLimit = document.querySelector("#daily-note-limit");
  const dailyTokenLimit = document.querySelector("#daily-token-limit");
  const attentionPosture = document.querySelector("#attention-posture");
  const quietHoursEnabled = document.querySelector("#quiet-hours-enabled");
  const quietHoursStart = document.querySelector("#quiet-hours-start");
  const quietHoursEnd = document.querySelector("#quiet-hours-end");
  const autonomyAvailability = document.querySelector("#autonomy-availability");
  const autonomySaveState = document.querySelector("#autonomy-save-state");
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
    const luna = state.ambient.luna || {};
    lunaEnabled.checked = luna.enabled === true;
    lunaFocus.value = luna.focus || "";
    lunaAvailability.textContent = availabilityText(luna.availability, luna);
    ambientProviderList.replaceChildren();
    ambientChannelList.replaceChildren();
    for (const provider of state.ambient.providers || []) appendAmbientProvider(provider);
    for (const channel of state.ambient.channels || []) appendAmbientChannel(channel);
    ambientEmptyState.hidden = Boolean(
      luna.enabled ||
        state.ambient.providers?.length ||
        state.ambient.channels?.length,
    );
  }

  function renderMailInput() {
    if (!state.mailInput) return;
    const inbox = state.mailInput;
    mailInputName.value = inbox.name || "Research Inbox";
    mailInputNameLabel.textContent = inbox.name || "Research Inbox";
    mailInputHost.value = inbox.host || "";
    mailInputPort.value = String(inbox.port || 993);
    mailInputUsername.value = inbox.username || "";
    mailInputFolder.value = inbox.folder || "INBOX";
    mailInputMaxMessages.value = String(inbox.maxMessages || 12);
    mailInputCredentialStore.value = inbox.credentialStore || "config_file";
    mailInputCredentialValue.value = "";
    mailInputAllowedSenders.value = (inbox.allowedSenders || []).join("\n");
    mailInputEnabled.checked = inbox.enabled === true;
    mailInputAvailability.textContent = availabilityText(inbox.availability, inbox);
    mailInputCredentialNote.textContent = credentialNote(inbox);
    const received = inbox.lastReceivedAt
      ? `上次接收 ${inbox.lastReceivedCount || 0} 封：${new Date(inbox.lastReceivedAt).toLocaleString()}`
      : "尚未接收到新的白名单邮件。";
    mailInputRuntimeNote.textContent = inbox.lastError ? `上次连接失败：${inbox.lastError}` : received;
  }

  function availabilityText(availability, runtime = {}) {
    if (runtime.lastError) return `上次失败：${runtime.lastError}`;
    if (availability === "ready") return "就绪";
    if (availability === "disabled") return "已关闭";
    if (availability === "incomplete") return "配置未完成";
    if (availability === "missing_credential") return "未配置密钥";
    if (availability === "credential_unavailable") return "密钥暂不可读取";
    if (availability === "unavailable") return "Codex 暂不可用";
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
    maxInputParallelism.value = String(state.autonomy.maxInputParallelism || 1);
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

  function renderAutonomy() {
    renderAutonomyConfig();
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
      luna: {
        enabled: lunaEnabled.checked,
        focus: lunaFocus.value.trim(),
      },
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
    event?.preventDefault();
    ambientSaveState.textContent = "保存中";
    try {
      state.ambient = await responseJson(
        await fetch("/api/ambient", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(ambientFormValue()),
        }),
        "探索通道保存失败",
      );
      renderAmbient();
      ambientSaveState.textContent = "已保存";
    } catch (error) {
      ambientSaveState.textContent = error.message;
    }
  }

  function mailInputFormValue() {
    return {
      enabled: mailInputEnabled.checked,
      name: mailInputName.value.trim(),
      host: mailInputHost.value.trim(),
      port: Number(mailInputPort.value),
      username: mailInputUsername.value.trim(),
      folder: mailInputFolder.value.trim(),
      credentialStore: mailInputCredentialStore.value,
      credentialValue: mailInputCredentialValue.value || null,
      allowedSenders: mailInputAllowedSenders.value
        .split(/[\n,，]/)
        .map((sender) => sender.trim())
        .filter(Boolean),
      maxMessages: Number(mailInputMaxMessages.value),
    };
  }

  async function saveMailInput(event) {
    event.preventDefault();
    mailInputSaveState.textContent = "保存中";
    try {
      state.mailInput = await responseJson(
        await fetch("/api/mail-input", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(mailInputFormValue()),
        }),
        "研究收件箱保存失败",
      );
      renderMailInput();
      mailInputSaveState.textContent = "已保存";
    } catch (error) {
      mailInputSaveState.textContent = error.message;
    }
  }

  async function saveAutonomy(event) {
    event.preventDefault();
    autonomySaveState.textContent = "保存中";
    const config = {
      enabled: autonomyEnabled.checked,
      intervalMinutes: Number(autonomyInterval.value),
      maxInputParallelism: Number(maxInputParallelism.value),
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
  }

  function openSettings(tab = "exploration") {
    renderCompute();
    renderAmbient();
    renderMailInput();
    renderAutonomy();
    renderBridge();
    computeSaveState.textContent = "";
    ambientSaveState.textContent = "";
    autonomySaveState.textContent = "";
    activateTab(normalizeSettingsTab(tab));
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
  mailInputForm.addEventListener("submit", saveMailInput);
  lunaEnabled.addEventListener("change", saveAmbient);
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
        name: "新的广域输入",
        model: "gpt-5-mini",
        focus: "Describe the independent external domain this role should observe.",
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
  quietHoursEnabled.addEventListener("change", toggleQuietInputs);
  for (const button of tabButtons) {
    button.addEventListener("click", () =>
      activateTab(button.dataset.settingsTab),
    );
  }
  document.querySelector("#open-settings").addEventListener("click", () => {
    openSettings("exploration");
  });

  return {
    render() {
      renderCompute();
      renderAmbient();
      renderMailInput();
      renderAutonomy();
      renderBridge();
    },
    renderAutonomy,
    renderAmbient,
    open: openSettings,
  };
}

function normalizeSettingsTab(tab) {
  return {
    appearance: "general",
    autonomy: "exploration",
    sources: "sources",
    cognition: "reflection",
    bridge: "system",
  }[tab] || tab;
}
