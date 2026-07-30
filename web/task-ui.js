import { responseJson } from "/presentation.js";

export function initTaskUi(state, insertIntoComposer, openSettings) {
  const dialog = document.querySelector("#task-dialog");
  const status = document.querySelector("#task-dialog-status");
  const list = document.querySelector("#task-list");
  const detail = document.querySelector("#task-detail");
  const openButton = document.querySelector("#open-tasks");
  let activeTaskId = null;

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
    title.textContent = "Codex 任务保持隔离";
    const description = document.createElement("p");
    description.textContent = "开启后仍只会在你点选任务时读取正文。";
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
      status.textContent = tasks.length
        ? `${tasks.length} 个近期任务 · 点选后读取正文`
        : "没有可见的近期任务";
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
    for (const row of list.querySelectorAll(".task-row")) {
      row.setAttribute("aria-current", String(row.dataset.taskId === taskId));
    }
    detail.innerHTML = '<div class="task-empty">正在读取任务</div>';
    try {
      const payload = await responseJson(
        await fetch(`/api/codex/tasks/${encodeURIComponent(taskId)}`),
        "无法读取任务正文",
      );
      if (activeTaskId === taskId) renderDetail(payload);
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
    const insert = document.createElement("button");
    insert.className = "primary-button";
    insert.type = "button";
    insert.textContent = "带入对话";
    insert.addEventListener("click", () => {
      insertIntoComposer(formatReference(payload));
      dialog.close();
    });
    header.append(heading, insert);
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

  return {
    open,
    configUpdated() {
      if (dialog.open && !state.bridge?.codexTaskAccess) renderDisabled();
    },
  };
}

function formatReference(payload) {
  const lines = [
    `我想接着讨论这个 Codex 任务：${payload.task.title}`,
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
