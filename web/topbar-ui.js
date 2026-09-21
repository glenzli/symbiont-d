export function connectionIssues(state) {
  const issues = [];
  const drive = state.driveInput;
  if (drive?.enabled && (
    drive.lastError ||
    ["failed", "invalid", "disconnected"].includes(drive.oauth?.status) ||
    ["incomplete", "missing_credential", "credential_unavailable"].includes(drive.availability)
  )) {
    issues.push({
      label: drive.lastError || drive.oauth?.error ? "Drive 需处理" : "Drive 未连接",
      detail: drive.lastError || drive.oauth?.error || "Google Drive 输入尚未就绪",
      settingsTab: "sources",
      sourceTab: "drive",
    });
  }
  const mail = state.mailInput;
  if (mail?.enabled && (
    mail.lastError ||
    ["incomplete", "missing_credential", "credential_unavailable"].includes(mail.availability)
  )) {
    issues.push({
      label: "邮箱需处理",
      detail: mail.lastError || "邮箱输入尚未就绪",
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
    alertLabel.textContent = first.label;
    alertCount.textContent = String(currentIssues.length);
    alertCount.hidden = currentIssues.length === 1;
    const details = currentIssues.map((issue) => `${issue.label}：${issue.detail}`).join("；");
    alert.title = `${details}。点此处理`;
    alert.setAttribute("aria-label", `${details}。打开设置处理`);
  }

  return { render };
}
