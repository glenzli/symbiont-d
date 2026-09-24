export function connectionIssues(state) {
  const issues = [];
  const drive = state.driveInput;
  const driveUnavailable = ["incomplete", "missing_credential", "credential_unavailable"].includes(drive?.availability);
  const driveReadFailure = Boolean(drive?.lastError && drive.consecutivePollFailures >= 2);
  const driveGrantRejected = drive?.requiresReauthorization === true;
  if (drive?.enabled && (driveUnavailable || driveReadFailure || driveGrantRejected)) {
    issues.push({
      sourceName: "Drive",
      label: driveUnavailable ? "Drive 未连接"
        : driveGrantRejected ? "Drive 授权失效" : "Drive 连续读取失败",
      detail: driveUnavailable
        ? drive.oauth?.error || "Google Drive 输入尚未就绪"
        : drive.lastError,
      settingsTab: "sources",
      sourceTab: "drive",
    });
  }
  const mail = state.mailInput;
  const mailUnavailable = ["incomplete", "missing_credential", "credential_unavailable"].includes(mail?.availability);
  const mailReadFailure = Boolean(mail?.lastError && mail.consecutivePollFailures >= 2);
  if (mail?.enabled && (mailUnavailable || mailReadFailure)) {
    issues.push({
      sourceName: "邮箱",
      label: mailUnavailable ? "邮箱未连接" : "邮箱连续读取失败",
      detail: mailUnavailable ? "邮箱输入尚未就绪" : mail.lastError,
      settingsTab: "sources",
      sourceTab: "mail",
    });
  }
  return issues;
}

export function initTopbarUi(state = {}, actions = {}) {
  const overflow = document.querySelector("#top-overflow");
  const alert = document.querySelector("#connection-alert");
  const alertLabel = document.querySelector("#connection-alert-label");
  const alertCount = document.querySelector("#connection-alert-count");
  let currentIssues = [];

  if (!overflow) return { render() {} };

  overflow.addEventListener("click", (event) => {
    if (event.target.closest("[data-top-menu-action]")) {
      overflow.open = false;
    }
  });

  document.addEventListener("click", (event) => {
    if (overflow.open && !overflow.contains(event.target)) overflow.open = false;
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") overflow.open = false;
  });

  alert?.addEventListener("click", () => {
    const issue = currentIssues[0];
    if (issue) actions.openSettings?.(issue.settingsTab, issue.sourceTab);
  });

  function render() {
    if (!alert) return;
    currentIssues = connectionIssues(state);
    alert.hidden = currentIssues.length === 0;
    if (!currentIssues.length) {
      alert.removeAttribute("title");
      alert.setAttribute("aria-label", "连接状态正常");
      return;
    }
    const [first] = currentIssues;
    alertLabel.textContent = currentIssues.length === 1
      ? first.label
      : `${currentIssues.map((issue) => issue.sourceName).join("、")}连接异常`;
    alertCount.textContent = String(currentIssues.length);
    alertCount.hidden = true;
    const details = currentIssues.map((issue) => `${issue.label}：${issue.detail}`).join("；");
    alert.title = `${details}。点此处理`;
    alert.setAttribute("aria-label", `${details}。打开设置处理`);
  }

  return { render };
}
