import { responseJson } from "/presentation.js";

export function initTaskUi(state, insertIntoComposer, openSettings) {
  const dialog = document.querySelector("#task-dialog");
  const status = document.querySelector("#task-dialog-status");
  const list = document.querySelector("#task-list");
  const detail = document.querySelector("#task-detail");
  const openButton = document.querySelector("#open-tasks");
  const targetTray = document.querySelector("#task-target-tray");
  const targetTitle = document.querySelector("#task-target-title");
  const targetScope = document.querySelector("#task-target-scope");
  const changeTarget = document.querySelector("#change-task-target");
  const clearTarget = document.querySelector("#clear-task-target");
  const handoffTray = document.querySelector("#handoff-tray");
  const handoffTitle = document.querySelector("#handoff-title");
  const handoffStatus = document.querySelector("#handoff-status");
  const openHandoff = document.querySelector("#open-handoff");
  let activeTaskId = null;
  let activePayload = null;
  let taskCount = 0;

  async function open() {
    dialog.showModal();
    if (!state.bridge?.codexTaskAccess) {
      renderDisabled();
      return;
    }
    await loadTasks();
  }

  function renderDisabled() {
    status.textContent = "访问未启用";
    list.replaceChildren();
    detail.replaceChildren();
    const empty = document.createElement("div");
    empty.className = "task-empty";
    const title = document.createElement("strong");
    title.textContent = "Codex 任务作为外部来源";
    const description = document.createElement("p");
    description.textContent = "开启后只会在你点选任务时读取正文；不会续写或改变原任务。";
    const enable = document.createElement("button");
    enable.className = "primary-button";
    enable.type = "button";
    enable.textContent = "打开连接设置";
    enable.addEventListener("click", () => {
      dialog.close();
      openSettings("bridge");
    });
    empty.append(title, description, enable);
    detail.append(empty);
  }

  async function loadTasks() {
    status.textContent = "正在读取任务索引";
    list.textContent = "";
    detail.innerHTML = '<div class="task-empty">选择一个任务</div>';
    try {
      const tasks = await responseJson(
        await fetch("/api/codex/tasks"),
        "无法读取 Codex 任务",
      );
      taskCount = tasks.length;
      renderStatus();
      renderList(tasks);
    } catch (error) {
      status.textContent = error.message;
      detail.innerHTML = `<div class="task-empty">${escapeText(error.message)}</div>`;
    }
  }

  function renderList(tasks) {
    list.replaceChildren();
    for (const task of tasks) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "task-row";
      button.dataset.taskId = task.id;
      const title = document.createElement("strong");
      title.textContent = task.title;
      const meta = document.createElement("span");
      meta.textContent = [
        shortPath(task.cwd),
        formatTaskTime(task.updatedAt),
      ].filter(Boolean).join(" · ");
      button.append(title, meta);
      button.addEventListener("click", () => loadTask(task.id));
      list.append(button);
    }
  }

  async function loadTask(taskId) {
    activeTaskId = taskId;
    activePayload = null;
    for (const row of list.querySelectorAll(".task-row")) {
      row.setAttribute("aria-current", String(row.dataset.taskId === taskId));
    }
    detail.innerHTML = '<div class="task-empty">正在读取任务</div>';
    try {
      const payload = await responseJson(
        await fetch(`/api/codex/tasks/${encodeURIComponent(taskId)}`),
        "无法读取任务正文",
      );
      if (activeTaskId === taskId) {
        activePayload = payload;
        renderDetail(payload);
      }
    } catch (error) {
      detail.innerHTML = `<div class="task-empty">${escapeText(error.message)}</div>`;
    }
  }

  function renderDetail(payload) {
    detail.replaceChildren();
    const header = document.createElement("header");
    header.className = "task-detail-header";
    const heading = document.createElement("div");
    const title = document.createElement("h3");
    title.textContent = payload.task.title;
    const path = document.createElement("p");
    path.textContent = payload.task.cwd || "未记录工作目录";
    heading.append(title, path);
    const actions = document.createElement("div");
    actions.className = "task-detail-actions";
    const insert = document.createElement("button");
    insert.className = "secondary-button";
    insert.type = "button";
    insert.textContent = "带入上下文";
    insert.addEventListener("click", () => {
      insertIntoComposer(formatReference(payload));
      dialog.close();
    });
    const scope = document.createElement("select");
    scope.className = "task-target-scope-select";
    scope.setAttribute("aria-label", "项目选择持续时间");
    scope.title = "项目选择持续时间";
    for (const [value, label] of [
      ["one_shot", "仅本轮"],
      ["topic", "当前话题"],
      ["pinned", "固定项目"],
    ]) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = label;
      scope.append(option);
    }
    const activeLease = state.bridge?.activeProjectLease;
    if (activeLease?.project?.cwd === payload.task.cwd) {
      scope.value = activeLease.scope;
    }
    const select = document.createElement("button");
    select.className = "primary-button";
    select.type = "button";
    select.textContent = "使用此项目";
    select.addEventListener("click", async () => {
      select.disabled = true;
      try {
        const response = await fetch(
          `/api/codex/tasks/${encodeURIComponent(payload.task.id)}/project`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ scope: scope.value }),
          },
        );
        state.bridge = await responseJson(response, "无法选择 Codex 项目");
        renderTarget();
        renderStatus();
        dialog.close();
      } catch (error) {
        status.textContent = error.message;
        select.disabled = false;
      }
    });
    actions.append(scope, select, insert);
    header.append(heading, actions);
    detail.append(header);

    const transcript = document.createElement("div");
    transcript.className = "task-transcript";
    if (!payload.messages.length) {
      transcript.innerHTML = '<div class="task-empty">这个任务没有可读取的对话正文</div>';
    }
    for (const message of payload.messages) {
      const item = document.createElement("article");
      item.className = "task-message";
      item.dataset.role = message.role;
      const meta = document.createElement("div");
      meta.className = "task-message-meta";
      meta.textContent = message.role === "user" ? "你" : "Codex";
      if (message.at) {
        meta.textContent += ` · ${formatTaskTime(message.at)}`;
      }
      const body = document.createElement("div");
      body.className = "task-message-body";
      body.textContent = message.text;
      item.append(meta, body);
      transcript.append(item);
    }
    detail.append(transcript);
    if (payload.truncated) {
      const note = document.createElement("p");
      note.className = "task-truncated";
      note.textContent = "这里只保留了近期且限长的对话内容。";
      detail.append(note);
    }
  }

  openButton.addEventListener("click", open);
  changeTarget.addEventListener("click", open);
  openHandoff.addEventListener("click", async () => {
    await open();
    const taskId = state.projectHandoffs?.[0]?.codexTaskId;
    if (taskId) await loadTask(taskId);
  });
  clearTarget.addEventListener("click", async () => {
    clearTarget.disabled = true;
    try {
      state.bridge = await responseJson(
        await fetch("/api/codex/project", { method: "DELETE" }),
        "无法清除项目选择",
      );
      renderTarget();
      renderStatus();
    } catch (error) {
      status.textContent = error.message;
    } finally {
      clearTarget.disabled = false;
    }
  });

  return {
    open,
    configUpdated() {
      renderTarget();
      renderHandoff();
      if (dialog.open && !state.bridge?.codexTaskAccess) renderDisabled();
      else if (dialog.open) {
        renderStatus();
        if (activePayload) renderDetail(activePayload);
      }
    },
    runtimeUpdated() {
      renderTarget();
      renderHandoff();
      if (dialog.open) renderStatus();
    },
  };

  function renderStatus() {
    const activeRun = (state.projectHandoffs || []).find((run) =>
      ["queued", "running"].includes(run.phase),
    );
    if (activeRun) {
      status.textContent =
        activeRun.currentActivity ||
        `正在交接 ${activeRun.project.title}`;
      return;
    }
    const lease = state.bridge?.activeProjectLease;
    if (lease) {
      status.textContent = `${scopeLabel(lease.scope)} · ${lease.project.title}${state.bridge.projectHandoffsEnabled ? " · 可新建任务" : " · 只读"}`;
      return;
    }
    status.textContent = taskCount
      ? `${taskCount} 个近期任务 · 点选后读取正文`
      : "没有可见的近期任务";
  }

  function renderTarget() {
    const lease = state.bridge?.activeProjectLease;
    targetTray.hidden = !lease;
    if (!lease) return;
    targetTitle.textContent = lease.project.title;
    targetScope.textContent = scopeLabel(lease.scope);
    targetTray.title = lease.expiresAt
      ? `关系将在 ${new Date(lease.expiresAt).toLocaleTimeString([], {
          hour: "2-digit",
          minute: "2-digit",
        })} 前保持`
      : "固定项目，直到手动清除";
  }

  function renderHandoff() {
    const handoff = state.projectHandoffs?.[0];
    handoffTray.hidden = !handoff;
    if (!handoff) return;
    handoffTitle.textContent = `Codex · ${handoff.project.title}`;
    handoffStatus.textContent = handoffStatusLabel(handoff);
    handoffTray.title = handoff.error || handoff.currentActivity || handoff.instruction;
  }
}

function scopeLabel(scope) {
  return (
    {
      one_shot: "仅本轮",
      topic: "当前话题",
      pinned: "固定项目",
    }[scope] || scope
  );
}

function handoffStatusLabel(handoff) {
  if (handoff.phase === "queued") return "等待创建新任务";
  if (handoff.phase === "running") return handoff.currentActivity || "正在执行";
  if (handoff.phase === "completed") return "新 Codex 任务已完成";
  if (handoff.phase === "failed") return handoff.error || "交接失败";
  if (handoff.phase === "interrupted") return "交接被中断";
  return handoff.phase || "交接记录";
}

function formatReference(payload) {
  const lines = [
    `这是我主动带入的外部 Codex 任务上下文（不会续写原任务）：${payload.task.title}`,
    payload.task.cwd ? `工作目录：${payload.task.cwd}` : "",
    "",
  ];
  for (const message of payload.messages) {
    lines.push(message.role === "user" ? "我：" : "Codex：", message.text, "");
  }
  const text = lines.filter((line, index) => line || index > 1).join("\n").trim();
  return text.length <= 11_500 ? text : `${text.slice(0, 11_499)}…`;
}

function shortPath(path) {
  if (!path) return "";
  const parts = path.split("/").filter(Boolean);
  return parts.slice(-2).join("/");
}

function formatTaskTime(seconds) {
  if (!seconds) return "";
  return new Date(seconds * 1000).toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function escapeText(value) {
  const span = document.createElement("span");
  span.textContent = value;
  return span.innerHTML;
}
