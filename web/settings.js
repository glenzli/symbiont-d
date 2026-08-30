import {
  millionsToTokens,
  responseJson,
  tokensToMillions,
} from "/presentation.js";

export function initSettings(state, actions = {}) {
  const dialog = document.querySelector("#settings-dialog");
  const settingsSaveState = document.querySelector("#settings-save-state");
  const settingsSave = document.querySelector("#settings-save");
  const computeForm = document.querySelector("#compute-form");
  const routingSelect = document.querySelector("#routing");
  const computeSaveState = settingsSaveState;
  const ambientForm = document.querySelector("#ambient-form");
  const ambientSaveState = settingsSaveState;
  const ambientEmptyState = document.querySelector("#ambient-empty-state");
  const lunaEnabled = document.querySelector("#luna-enabled");
  const lunaOutputLanguage = document.querySelector("#luna-output-language");
  const lunaFocus = document.querySelector("#luna-focus");
  const lunaAvailability = document.querySelector("#luna-availability");
  const ambientProviderList = document.querySelector("#ambient-provider-list");
  const ambientProviderTemplate = document.querySelector("#ambient-provider-template");
  const addAmbientProvider = document.querySelector("#add-ambient-provider");
  const ambientChannelList = document.querySelector("#ambient-channel-list");
  const ambientChannelTemplate = document.querySelector("#ambient-channel-template");
  const addAmbientChannel = document.querySelector("#add-ambient-channel");
  const driveInputForm = document.querySelector("#drive-input-form");
  const driveInputName = document.querySelector("#drive-input-name");
  const driveInputNameLabel = document.querySelector("#drive-input-name-label");
  const driveInputFolderId = document.querySelector("#drive-input-folder-id");
  const driveInputFileSelection = document.querySelector("#drive-input-file-selection");
  const driveInputFileNamePattern = document.querySelector("#drive-input-file-name-pattern");
  const driveInputMaxFiles = document.querySelector("#drive-input-max-files");
  const driveInputCredentialStore = document.querySelector("#drive-input-credential-store");
  const driveInputCredentialValue = document.querySelector("#drive-input-credential-value");
  const driveInputEnabled = document.querySelector("#drive-input-enabled");
  const driveInputAvailability = document.querySelector("#drive-input-availability");
  const driveInputCredentialNote = document.querySelector("#drive-input-credential-note");
  const driveInputRuntimeNote = document.querySelector("#drive-input-runtime-note");
  const driveInputSaveState = document.querySelector("#drive-input-save-state");
  const driveInputConnectOAuth = document.querySelector("#drive-input-connect-oauth");
  const driveInputDisconnectOAuth = document.querySelector("#drive-input-disconnect-oauth");
  const driveInputTestConnection = document.querySelector("#drive-input-test-connection");
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
  const mailInputSaveState = settingsSaveState;
  const mailInputTestConnection = document.querySelector("#mail-input-test-connection");
  const audioTranscriptionLanguage = document.querySelector("#audio-transcription-language");
  const audioTranscriptionEnabled = document.querySelector("#audio-transcription-enabled");
  const audioTranscriptionAvailability = document.querySelector("#audio-transcription-availability");
  const audioTranscriptionRuntimeNote = document.querySelector("#audio-transcription-runtime-note");
  const audioTranscriptionSaveState = settingsSaveState;
  const computePolicyList = document.querySelector("#compute-policy-list");
  const computePolicyTemplate = document.querySelector(
    "#compute-policy-template",
  );
  const addComputePolicy = document.querySelector("#add-compute-policy");
  const modelParticipantList = document.querySelector("#model-participant-list");
  const modelParticipantTemplate = document.querySelector("#model-participant-template");
  const addModelParticipant = document.querySelector("#add-model-participant");
  const autonomyForm = document.querySelector("#autonomy-form");
  const autonomyEnabled = document.querySelector("#autonomy-enabled");
  const attackerEnabled = document.querySelector("#attacker-enabled");
  const autonomyInterval = document.querySelector("#autonomy-interval");
  const maxInputParallelism = document.querySelector("#max-input-parallelism");
  const signalRetentionDays = document.querySelector("#signal-retention-days");
  const dailyInterruptLimit = document.querySelector("#daily-interrupt-limit");
  const dailyNoteLimit = document.querySelector("#daily-note-limit");
  const dailyTokenLimit = document.querySelector("#daily-token-limit");
  const attentionPosture = document.querySelector("#attention-posture");
  const quietHoursEnabled = document.querySelector("#quiet-hours-enabled");
  const quietHoursStart = document.querySelector("#quiet-hours-start");
  const quietHoursEnd = document.querySelector("#quiet-hours-end");
  const autonomyAvailability = document.querySelector("#autonomy-availability");
  const autonomySaveState = settingsSaveState;
  const bridgeForm = document.querySelector("#bridge-form");
  const codexTaskAccess = document.querySelector("#codex-task-access");
  const bridgeSaveState = settingsSaveState;
  const tabButtons = [...dialog.querySelectorAll("[data-settings-tab]")];
  const tabPanels = [...dialog.querySelectorAll("[data-settings-panel]")];
  const sourceTabButtons = [
    ...dialog.querySelectorAll("[data-source-settings-tab]"),
  ];
  const sourceTabPanels = [
    ...dialog.querySelectorAll("[data-source-settings-panel]"),
  ];
  let activeSettingsTab = "exploration";
  let activeSourceSettingsTab = "ambient";
  let driveInputTestController = null;
  let driveInputOAuthPollTimer = null;
  let mailInputTestController = null;

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
    renderModelCouncil();
  }

  function renderModelCouncil() {
    modelParticipantList.replaceChildren();
    for (const participant of state.modelCouncil?.participants || []) {
      appendModelParticipant(participant);
    }
  }

  function appendModelParticipant(participant = {}, focus = false) {
    const row = modelParticipantTemplate.content.firstElementChild.cloneNode(true);
    const field = (name) => row.querySelector(`[data-participant-field="${name}"]`);
    field("id").value = participant.id || "new-peer";
    field("name").value = participant.name || "新模型";
    field("avatar").value = participant.avatar || "◌";
    field("enabled").checked = participant.enabled === true;
    field("transport").value = participant.transport || "infer_runtime";
    field("model").value = participant.model || "language.respond";
    field("maxOutputTokens").value = String(participant.maxOutputTokens || 900);
    field("routeKind").value = participant.routeKind || "automatic";
    field("routeId").value = participant.routeId || "";
    field("role").value = participant.role || "提供独立视角，指出主判断可能忽略的假设。";
    const provider = field("providerId");
    provider.replaceChildren(
      ...(state.ambient?.providers || []).map((item) => {
        const option = document.createElement("option");
        option.value = item.id;
        option.textContent = item.id;
        return option;
      }),
    );
    provider.value = participant.providerId || provider.options[0]?.value || "";
    row.querySelector(".model-participant-title").textContent = field("name").value;
    modelParticipantList.append(row);
    updateModelParticipantFields(row);
    if (focus) field("name").focus();
  }

  function updateModelParticipantFields(row) {
    const transport = row.querySelector('[data-participant-field="transport"]').value;
    const routeKind = row.querySelector('[data-participant-field="routeKind"]').value;
    const infer = transport === "infer_runtime";
    row.querySelector(".participant-provider-field").hidden = infer;
    row.querySelector(".participant-route-kind-field").hidden = !infer;
    row.querySelector(".participant-route-id-field").hidden = !infer || routeKind === "automatic";
  }

  function modelCouncilFormValue() {
    return {
      participants: [...modelParticipantList.querySelectorAll("[data-model-participant]")].map((row) => {
        const field = (name) => row.querySelector(`[data-participant-field="${name}"]`);
        const infer = field("transport").value === "infer_runtime";
        const routeKind = infer ? field("routeKind").value : "automatic";
        return {
          id: field("id").value.trim(),
          enabled: field("enabled").checked,
          name: field("name").value.trim(),
          role: field("role").value.trim(),
          avatar: field("avatar").value.trim(),
          transport: field("transport").value,
          providerId: infer ? null : field("providerId").value,
          model: field("model").value.trim(),
          routeKind,
          routeId: infer && routeKind !== "automatic" ? field("routeId").value.trim() : null,
          maxOutputTokens: Number(field("maxOutputTokens").value || 900),
        };
      }),
    };
  }

  function renderAmbient() {
    if (!state.ambient) return;
    const luna = state.ambient.luna || {};
    lunaEnabled.checked = luna.enabled === true;
    lunaOutputLanguage.value = luna.outputLanguage || "interface";
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

  function renderDriveInput() {
    if (!state.driveInput) return;
    const drive = state.driveInput;
    driveInputName.value = drive.name || "Google Drive Inbox";
    driveInputNameLabel.textContent = drive.name || "Google Drive Inbox";
    driveInputFolderId.value = drive.folderId || "";
    driveInputFileSelection.value = drive.fileSelection || "pattern";
    driveInputFileNamePattern.value = drive.fileNamePattern || "Digest_*.md";
    driveInputFileNamePattern.disabled = driveInputFileSelection.value === "all";
    driveInputMaxFiles.value = String(drive.maxFiles || 12);
    driveInputCredentialStore.value = drive.credentialStore || "config_file";
    driveInputCredentialValue.value = "";
    driveInputEnabled.checked = drive.enabled === true;
    driveInputAvailability.textContent = availabilityText(drive.availability, drive);
    renderDriveInputOAuth(drive);
    const received = drive.lastReceivedAt
      ? `上次接收 ${drive.lastReceivedCount || 0} 个文件：${new Date(drive.lastReceivedAt).toLocaleString()}`
      : "尚未接收到新的 Drive 文件。";
    const intake = drive.lastSucceededAt
      ? `上次读取：列出 ${drive.lastListedFileCount || 0}，匹配 ${drive.lastMatchingFileCount || 0}，选择 ${drive.lastSelectedFileCount || 0}，读取 ${drive.lastFetchedFileCount || 0}。`
      : "";
    driveInputRuntimeNote.textContent = drive.lastError
      ? `上次连接失败：${drive.lastError}`
      : `${received}${intake ? ` ${intake}` : ""}`;
  }

  function renderDriveInputOAuth(drive) {
    const oauth = drive.oauth || {};
    const waiting = oauth.status === "waiting";
    const connected = oauth.status === "connected";
    driveInputConnectOAuth.textContent = waiting
      ? "取消连接"
      : connected
        ? "重新连接"
        : "连接 Google Drive";
    driveInputDisconnectOAuth.hidden = !connected;
    driveInputTestConnection.disabled = waiting;
    if (waiting) {
      driveInputCredentialNote.textContent = "正在等待你在浏览器中完成 Google 授权。";
    } else if (connected) {
      driveInputCredentialNote.textContent = oauth.account
        ? `已连接个人账号：${oauth.account}`
        : "个人 Google Drive 已连接。";
    } else if (oauth.status === "failed" || oauth.status === "invalid") {
      driveInputCredentialNote.textContent = `授权失败：${oauth.error || "请重新连接 Google Drive"}`;
    } else {
      driveInputCredentialNote.textContent = "尚未连接个人 Google Drive。";
    }
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
    const intake = inbox.lastSucceededAt
      ? `上次读取：搜索 ${inbox.lastSearchableMessageCount || 0}，本轮选择 ${inbox.lastSelectedMessageCount || 0}，抓取 ${inbox.lastFetchedMessageCount || 0}，正文 ${inbox.lastBodyMessageCount || 0}，解析 ${inbox.lastParsedMessageCount || 0}，白名单 ${inbox.lastAllowedMessageCount || 0}。`
      : "";
    mailInputRuntimeNote.textContent = inbox.lastError
      ? `上次连接失败：${inbox.lastError}`
      : `${received}${intake ? ` ${intake}` : ""}`;
  }

  function renderAudioTranscription() {
    if (!state.audioTranscription) return;
    const transcription = state.audioTranscription;
    audioTranscriptionLanguage.value = transcription.language || "zh";
    audioTranscriptionEnabled.checked = transcription.enabled === true;
    audioTranscriptionAvailability.textContent = transcriptionStatusText(transcription);
    audioTranscriptionRuntimeNote.textContent = transcriptionRuntimeNote(transcription);
  }

  function transcriptionStatusText(transcription) {
    if (transcription.availability === "ready") return "已自动连接";
    if (transcription.availability === "disabled") return "已关闭";
    if (transcription.availability === "missing_credential") return "等待本机授权";
    if (transcription.availability === "credential_unavailable") return "本机凭据暂不可用";
    if (transcription.availability === "endpoint_unavailable") return "正在等待本地服务";
    return "暂不可用";
  }

  function transcriptionRuntimeNote(transcription) {
    if (transcription.availability === "missing_credential") {
      return "尚未获得本机访问凭据；请在 Infer Console 的 Apps & Access 中连接 symbiont-d。";
    }
    if (transcription.availability === "credential_unavailable") {
      return "本机访问凭据暂时不可读取，恢复后会自动重连。";
    }
    if (transcription.availability === "endpoint_unavailable") {
      return "暂未发现本地转写服务，服务恢复后会自动重连。";
    }
    return "服务地址由本机自动发现，无需手动配置。";
  }

  function availabilityText(availability, runtime = {}) {
    if (runtime.lastError) return `上次失败：${runtime.lastError}`;
    if (availability === "ready") return "就绪";
    if (availability === "disabled") return "已关闭";
    if (availability === "incomplete") return "配置未完成";
    if (availability === "missing_credential") return "未配置密钥";
    if (availability === "credential_unavailable") return "密钥暂不可读取";
    if (availability === "endpoint_unavailable") return "本地服务地址不可用";
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
    attackerEnabled.checked = state.autonomy.attackerEnabled !== false;
    autonomyInterval.value = String(state.autonomy.intervalMinutes);
    maxInputParallelism.value = String(state.autonomy.maxInputParallelism || 1);
    signalRetentionDays.value = String(state.signalRetention?.retentionDays ?? 7);
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
        outputLanguage: lunaOutputLanguage.value,
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
    event?.preventDefault();
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
      state.modelCouncil = await responseJson(
        await fetch("/api/model-council", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(modelCouncilFormValue()),
        }),
        "潜水模型保存失败",
      );
      renderCompute();
      window.dispatchEvent(new Event("symbiont:model-council-updated"));
      computeSaveState.textContent = "已保存";
      return true;
    } catch (error) {
      computeSaveState.textContent = error.message;
      return false;
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
      return true;
    } catch (error) {
      ambientSaveState.textContent = error.message;
      return false;
    }
  }

  function driveInputFormValue() {
    return {
      enabled: driveInputEnabled.checked,
      name: driveInputName.value.trim(),
      folderId: driveInputFolderId.value.trim(),
      fileSelection: driveInputFileSelection.value,
      fileNamePattern: driveInputFileNamePattern.value.trim(),
      credentialStore: driveInputCredentialStore.value,
      credentialValue: driveInputCredentialValue.value || null,
      maxFiles: Number(driveInputMaxFiles.value),
    };
  }

  async function persistDriveInput() {
    return responseJson(
      await fetch("/api/drive-input", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(driveInputFormValue()),
      }),
      "Google Drive Inbox 保存失败",
    );
  }

  async function saveDriveInput(event) {
    event?.preventDefault();
    driveInputSaveState.textContent = "保存中";
    try {
      state.driveInput = await persistDriveInput();
      renderDriveInput();
      driveInputSaveState.textContent = "已保存";
      return true;
    } catch (error) {
      driveInputSaveState.textContent = error.message;
      return false;
    }
  }

  async function testDriveInputConnection() {
    if (driveInputTestController) {
      driveInputTestConnection.disabled = true;
      driveInputSaveState.textContent = "正在停止连接测试…";
      try {
        await fetch("/api/drive-input/test/cancel", { method: "POST" });
      } finally {
        driveInputTestController.abort();
      }
      return;
    }
    const controller = new AbortController();
    driveInputTestController = controller;
    driveInputTestConnection.textContent = "停止测试";
    driveInputSaveState.textContent = "正在验证认证、文件筛选、下载与解析…";
    try {
      const result = await responseJson(
        await fetch("/api/drive-input/test", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(driveInputFormValue()),
          signal: controller.signal,
        }),
        "Google Drive Inbox 连接测试失败",
      );
      driveInputSaveState.textContent = `读取正常：列出 ${result.listedFileCount || 0}，匹配 ${result.matchingFileCount || 0}，选择 ${result.selectedFileCount || 0}，读取 ${result.fetchedFileCount || 0}，拆分候选 ${result.candidateCount || 0}；尚未保存`;
    } catch (error) {
      driveInputSaveState.textContent = controller.signal.aborted
        ? "连接测试已取消"
        : `连接失败：${error.message}`;
    } finally {
      if (driveInputTestController === controller) {
        driveInputTestController = null;
        driveInputTestConnection.disabled = false;
        driveInputTestConnection.textContent = "测试";
      }
    }
  }

  function stopDriveInputOAuthPolling() {
    if (driveInputOAuthPollTimer) window.clearTimeout(driveInputOAuthPollTimer);
    driveInputOAuthPollTimer = null;
  }

  async function pollDriveInputOAuth() {
    stopDriveInputOAuthPolling();
    try {
      const store = encodeURIComponent(driveInputCredentialStore.value);
      state.driveInput = await responseJson(
        await fetch(`/api/drive-input/oauth/status?credentialStore=${store}`),
        "读取 Google Drive 授权状态失败",
      );
      renderDriveInputOAuth(state.driveInput);
      const status = state.driveInput.oauth?.status;
      if (status === "connected") {
        driveInputCredentialValue.value = "";
        driveInputSaveState.textContent = "Google Drive 已连接；可以先测试，再保存当前页";
        return;
      }
      if (status === "failed" || status === "invalid" || status === "disconnected") {
        driveInputSaveState.textContent = state.driveInput.oauth?.error
          ? `连接失败：${state.driveInput.oauth.error}`
          : "Google Drive 尚未连接";
        return;
      }
      driveInputOAuthPollTimer = window.setTimeout(pollDriveInputOAuth, 1000);
    } catch (error) {
      driveInputSaveState.textContent = error.message;
      driveInputOAuthPollTimer = window.setTimeout(pollDriveInputOAuth, 2000);
    }
  }

  async function connectDriveInputOAuth() {
    if (state.driveInput?.oauth?.status === "waiting") {
      await fetch("/api/drive-input/oauth/cancel", { method: "POST" });
      stopDriveInputOAuthPolling();
      const store = encodeURIComponent(driveInputCredentialStore.value);
      state.driveInput = await responseJson(
        await fetch(`/api/drive-input/oauth/status?credentialStore=${store}`),
        "取消 Google Drive 授权失败",
      );
      renderDriveInputOAuth(state.driveInput);
      driveInputSaveState.textContent = "已取消 Google Drive 连接";
      return;
    }
    driveInputConnectOAuth.disabled = true;
    driveInputSaveState.textContent = "正在准备 Google 授权…";
    try {
      const result = await responseJson(
        await fetch("/api/drive-input/oauth/start", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            credentialStore: driveInputCredentialStore.value,
            credentialValue: driveInputCredentialValue.value || null,
          }),
        }),
        "启动 Google Drive 授权失败",
      );
      window.open(result.authorizationUrl, "_blank", "noopener,noreferrer");
      state.driveInput.oauth = {
        status: "waiting",
        account: null,
        expiresAt: result.expiresAt,
        error: null,
      };
      renderDriveInputOAuth(state.driveInput);
      driveInputSaveState.textContent = "请在浏览器中选择个人 Google 账号并授权";
      void pollDriveInputOAuth();
    } catch (error) {
      driveInputSaveState.textContent = error.message;
    } finally {
      driveInputConnectOAuth.disabled = false;
    }
  }

  async function disconnectDriveInputOAuth() {
    driveInputDisconnectOAuth.disabled = true;
    try {
      state.driveInput = await responseJson(
        await fetch("/api/drive-input/oauth/disconnect", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            credentialStore: driveInputCredentialStore.value,
          }),
        }),
        "断开 Google Drive 失败",
      );
      renderDriveInputOAuth(state.driveInput);
      driveInputSaveState.textContent = "已移除本机 Google Drive 授权";
    } catch (error) {
      driveInputSaveState.textContent = error.message;
    } finally {
      driveInputDisconnectOAuth.disabled = false;
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

  async function persistMailInput() {
    return responseJson(
      await fetch("/api/mail-input", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(mailInputFormValue()),
      }),
      "研究收件箱保存失败",
    );
  }

  async function saveMailInput(event) {
    event?.preventDefault();
    mailInputSaveState.textContent = "保存中";
    try {
      state.mailInput = await persistMailInput();
      renderMailInput();
      mailInputSaveState.textContent = "已保存";
      return true;
    } catch (error) {
      mailInputSaveState.textContent = error.message;
      return false;
    }
  }

  async function testMailInputConnection() {
    if (mailInputTestController) {
      mailInputTestConnection.disabled = true;
      mailInputSaveState.textContent = "正在停止连接测试…";
      try {
        await fetch("/api/mail-input/test/cancel", { method: "POST" });
      } finally {
        mailInputTestController.abort();
      }
      return;
    }
    const controller = new AbortController();
    mailInputTestController = controller;
    mailInputTestConnection.textContent = "停止测试";
    mailInputSaveState.textContent = "正在验证连接、正文抓取与解析…";
    try {
      const result = await responseJson(
        await fetch("/api/mail-input/test", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(mailInputFormValue()),
          signal: controller.signal,
        }),
        "研究收件箱连接测试失败",
      );
      mailInputSaveState.textContent = `读取正常：「${result.folder}」共 ${result.messageCount || 0} 封，抓取 ${result.fetchedMessageCount || 0}，解析 ${result.parsedMessageCount || 0}，白名单 ${result.allowedMessageCount || 0}，拆分候选 ${result.candidateCount || 0}；尚未保存`;
    } catch (error) {
      mailInputSaveState.textContent = controller.signal.aborted
        ? "连接测试已取消"
        : `连接失败：${error.message}`;
    } finally {
      if (mailInputTestController === controller) {
        mailInputTestController = null;
        mailInputTestConnection.disabled = false;
        mailInputTestConnection.textContent = "测试";
      }
    }
  }

  function audioTranscriptionFormValue() {
    const current = state.audioTranscription || {};
    return {
      enabled: audioTranscriptionEnabled.checked,
      baseUrl: "",
      language: audioTranscriptionLanguage.value.trim(),
      credentialStore: current.credentialStore || "config_file",
      credentialValue: null,
    };
  }

  async function saveAudioTranscription(event) {
    event?.preventDefault();
    audioTranscriptionSaveState.textContent = "保存中";
    try {
      state.audioTranscription = await responseJson(
        await fetch("/api/audio-transcription", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(audioTranscriptionFormValue()),
        }),
        "本地语音转写保存失败",
      );
      renderAudioTranscription();
      audioTranscriptionSaveState.textContent = "已保存";
      return true;
    } catch (error) {
      audioTranscriptionSaveState.textContent = error.message;
      return false;
    }
  }

  async function saveAutonomy(event) {
    event?.preventDefault();
    autonomySaveState.textContent = "保存中";
    const config = {
      enabled: autonomyEnabled.checked,
      attackerEnabled: attackerEnabled.checked,
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
      const [autonomy, signalRetention] = await Promise.all([
        responseJson(
          await fetch("/api/autonomy", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(config),
          }),
          "保存失败",
        ),
        responseJson(
          await fetch("/api/signal-retention", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              retentionDays: Number(signalRetentionDays.value),
            }),
          }),
          "保存外部输入保留期失败",
        ),
      ]);
      state.autonomy = autonomy;
      state.signalRetention = signalRetention;
      state.autonomyPermitted =
        state.profile.status === "ready" && state.autonomy.enabled;
      autonomySaveState.textContent = "已保存";
      renderAutonomy();
      return true;
    } catch (error) {
      autonomySaveState.textContent = error.message;
      return false;
    }
  }

  async function saveBridge(event) {
    event?.preventDefault();
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
      return true;
    } catch (error) {
      bridgeSaveState.textContent = error.message;
      return false;
    }
  }

  async function saveCurrentSettings() {
    if (activeSettingsTab === "general") {
      settingsSave.disabled = true;
      const identitySaved = await actions.saveIdentity?.();
      const rolesSaved = identitySaved === false
        ? false
        : await actions.saveInputRoles?.();
      settingsSave.disabled = false;
      settingsSaveState.textContent = rolesSaved === false ? "保存失败" : "已保存";
      return;
    }
    if (activeSettingsTab === "exploration") {
      await saveAutonomy();
      return;
    }
    if (activeSettingsTab === "sources") {
      if (activeSourceSettingsTab === "ambient") await saveAmbient();
      if (activeSourceSettingsTab === "drive") await saveDriveInput();
      if (activeSourceSettingsTab === "mail") await saveMailInput();
      return;
    }
    if (activeSettingsTab === "reflection") {
      window.dispatchEvent(new Event("symbiont:save-reflection-settings"));
      return;
    }
    if (activeSettingsTab === "models") {
      await saveCompute();
      return;
    }
    if (activeSettingsTab === "system") {
      if (await saveBridge()) await saveAudioTranscription();
    }
  }

  function activateTab(name) {
    activeSettingsTab = name;
    settingsSave.disabled = false;
    settingsSave.textContent = "保存当前页";
    for (const button of tabButtons) {
      button.setAttribute(
        "aria-selected",
        String(button.dataset.settingsTab === name),
      );
    }
    for (const panel of tabPanels) {
      panel.hidden = panel.dataset.settingsPanel !== name;
    }
    if (name === "general") {
      void actions.refreshInputRoles?.();
    }
  }

  function activateSourceTab(name) {
    activeSourceSettingsTab = sourceTabPanels.some(
      (panel) => panel.dataset.sourceSettingsPanel === name,
    )
      ? name
      : "ambient";
    for (const button of sourceTabButtons) {
      const selected = button.dataset.sourceSettingsTab === activeSourceSettingsTab;
      button.setAttribute("aria-selected", String(selected));
      button.tabIndex = selected ? 0 : -1;
    }
    for (const panel of sourceTabPanels) {
      panel.hidden = panel.dataset.sourceSettingsPanel !== activeSourceSettingsTab;
    }
  }

  function openSettings(tab = "exploration") {
    renderCompute();
    renderAmbient();
    renderDriveInput();
    renderMailInput();
    renderAudioTranscription();
    renderAutonomy();
    renderBridge();
    settingsSaveState.textContent = "";
    activateTab(normalizeSettingsTab(tab));
    activateSourceTab(activeSourceSettingsTab);
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
  driveInputForm.addEventListener("submit", saveDriveInput);
  driveInputConnectOAuth.addEventListener("click", connectDriveInputOAuth);
  driveInputDisconnectOAuth.addEventListener("click", disconnectDriveInputOAuth);
  driveInputTestConnection.addEventListener("click", testDriveInputConnection);
  driveInputFileSelection.addEventListener("change", () => {
    driveInputFileNamePattern.disabled = driveInputFileSelection.value === "all";
  });
  mailInputForm.addEventListener("submit", saveMailInput);
  mailInputTestConnection.addEventListener("click", testMailInputConnection);
  settingsSave.addEventListener("click", saveCurrentSettings);
  dialog.addEventListener("input", (event) => {
    if (event.target.matches("input, select, textarea")) {
      settingsSaveState.textContent = "有未保存的更改";
    }
  });
  lunaEnabled.addEventListener("change", () => {
    settingsSaveState.textContent = "有未保存的更改";
  });
  addComputePolicy.addEventListener("click", () => appendComputePolicy({}, true));
  computePolicyList.addEventListener("click", (event) => {
    const button = event.target.closest(".remove-compute-policy");
    if (button) button.closest(".compute-policy-row").remove();
  });
  addModelParticipant.addEventListener("click", () => appendModelParticipant({}, true));
  modelParticipantList.addEventListener("click", (event) => {
    const button = event.target.closest(".remove-model-participant");
    if (button) button.closest("[data-model-participant]").remove();
  });
  modelParticipantList.addEventListener("change", (event) => {
    const row = event.target.closest("[data-model-participant]");
    if (row) updateModelParticipantFields(row);
  });
  modelParticipantList.addEventListener("input", (event) => {
    const row = event.target.closest("[data-model-participant]");
    if (row && event.target.matches('[data-participant-field="name"]')) {
      row.querySelector(".model-participant-title").textContent = event.target.value || "参与模型";
    }
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
  for (const button of sourceTabButtons) {
    button.addEventListener("click", () =>
      activateSourceTab(button.dataset.sourceSettingsTab),
    );
    button.addEventListener("keydown", (event) => {
      if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
        return;
      }
      event.preventDefault();
      const current = sourceTabButtons.indexOf(button);
      const next = event.key === "Home"
        ? 0
        : event.key === "End"
          ? sourceTabButtons.length - 1
          : (current + (event.key === "ArrowUp" || event.key === "ArrowLeft" ? -1 : 1) + sourceTabButtons.length) % sourceTabButtons.length;
      const target = sourceTabButtons[next];
      activateSourceTab(target.dataset.sourceSettingsTab);
      target.focus();
    });
  }
  document.querySelector("#open-settings").addEventListener("click", () => {
    openSettings("exploration");
  });

  return {
    render() {
      renderCompute();
      renderAmbient();
      renderDriveInput();
      renderMailInput();
      renderAudioTranscription();
      renderAutonomy();
      renderBridge();
    },
    renderAutonomy,
    renderAmbient,
    renderDriveInput,
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
