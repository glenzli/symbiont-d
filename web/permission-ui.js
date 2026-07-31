export function initPermissionUi(state) {
  const center = document.querySelector("#permission-center");
  const list = document.querySelector("#permission-list");
  const status = document.querySelector("#permission-status");

  function render() {
    const requests = state.permissions || [];
    center.hidden = requests.length === 0;
    list.replaceChildren(...requests.map(renderRequest));
    status.textContent = requests.length
      ? `${requests.length} 项操作等待确认`
      : "";
  }

  function renderRequest(request) {
    const article = document.createElement("article");
    article.className = "permission-request";
    article.dataset.permissionId = request.id;

    const header = document.createElement("header");
    const identity = document.createElement("div");
    const eyebrow = document.createElement("small");
    const title = document.createElement("strong");
    const expiry = document.createElement("time");
    eyebrow.textContent =
      request.source === "codex" ? "CODEX PERMISSION" : "SYMBIONT PERMISSION";
    title.textContent = request.title;
    expiry.dateTime = request.expiresAt;
    expiry.textContent = `等待至 ${new Date(request.expiresAt).toLocaleTimeString(
      [],
      { hour: "2-digit", minute: "2-digit" },
    )}`;
    identity.append(eyebrow, title);
    header.append(identity, expiry);
    article.append(header);

    if (request.reason) {
      const reason = document.createElement("p");
      reason.className = "permission-reason";
      reason.textContent = request.reason;
      article.append(reason);
    }

    const facts = document.createElement("dl");
    facts.className = "permission-facts";
    if (request.host) {
      appendFact(
        facts,
        request.kind === "mcpElicitation" ? "地址" : "目标",
        request.host,
        request.kind === "mcpElicitation" ? request.host : null,
      );
    }
    if (request.protocol) appendFact(facts, "协议", request.protocol);
    if (request.cwd) appendFact(facts, "位置", request.cwd);
    if (facts.childElementCount) article.append(facts);

    if (request.command) {
      const command = document.createElement("pre");
      command.className = "permission-command";
      command.textContent = request.command;
      article.append(command);
    }

    const details = document.createElement("details");
    details.className = "permission-details";
    const summary = document.createElement("summary");
    const payload = document.createElement("pre");
    summary.textContent = "查看完整请求";
    payload.textContent = JSON.stringify(request.details, null, 2);
    details.append(summary, payload);
    article.append(details);

    const actions = document.createElement("div");
    actions.className = "permission-actions";
    if (request.allowAccept) {
      actions.append(
        actionButton("允许这一次", "accept", "primary-button"),
      );
    }
    if (request.allowAccept && request.allowSession) {
      actions.append(actionButton("本次会话允许", "acceptForSession"));
    }
    actions.append(actionButton("拒绝", "decline"));
    if (request.allowCancel) {
      actions.append(actionButton("停止操作", "cancel", "danger-button"));
    }
    article.append(actions);
    return article;

    function actionButton(label, decision, className = "secondary-button") {
      const button = document.createElement("button");
      button.type = "button";
      button.className = className;
      button.textContent = label;
      button.addEventListener("click", () =>
        resolve(request.id, decision, article),
      );
      return button;
    }
  }

  async function resolve(id, decision, article) {
    const buttons = [...article.querySelectorAll("button")];
    buttons.forEach((button) => {
      button.disabled = true;
    });
    status.textContent = "正在提交决定";
    try {
      const response = await fetch(
        `/api/permissions/${encodeURIComponent(id)}`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ decision }),
        },
      );
      const payload = await response.json();
      if (!response.ok) {
        throw new Error(payload.error || "权限请求已经失效");
      }
      state.permissions = (state.permissions || []).filter(
        (request) => request.id !== id,
      );
      render();
    } catch (error) {
      status.textContent = error.message;
      buttons.forEach((button) => {
        button.disabled = false;
      });
    }
  }

  return { render };
}

function appendFact(list, label, value, href = null) {
  const term = document.createElement("dt");
  const detail = document.createElement("dd");
  term.textContent = label;
  if (href) {
    const link = document.createElement("a");
    link.href = href;
    link.target = "_blank";
    link.rel = "noreferrer";
    link.textContent = value;
    detail.append(link);
  } else {
    detail.textContent = value;
  }
  list.append(term, detail);
}
