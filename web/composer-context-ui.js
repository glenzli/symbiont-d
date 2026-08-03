import { responseJson } from "/presentation.js";

const MAX_CODEX_CONTEXTS = 2;
const SESSION_CACHE_KEY = "symbiont.codex-task-summaries.v1";

export function initComposerContextUi({ state, chooseImage, notify, openSettings }) {
  const trigger = document.querySelector("#add-context");
  const menu = document.querySelector("#composer-context-menu");
  const options = document.querySelector("#composer-context-options");
  const image = document.querySelector("#add-context-image");
  const codex = document.querySelector("#add-context-codex");
  const picker = document.querySelector("#codex-context-picker");
  const back = document.querySelector("#codex-context-back");
  const filter = document.querySelector("#codex-context-filter");
  const status = document.querySelector("#codex-context-status");
  const list = document.querySelector("#codex-context-list");
  const tray = document.querySelector("#codex-context-tray");
  let tasks = loadSessionTasks();
  let selected = [];
  let loading = null;

  trigger.addEventListener("click", () => {
    if (menu.hidden) openOptions();
    else close();
  });
  image.addEventListener("click", () => {
    close();
    chooseImage();
  });
  codex.addEventListener("click", openPicker);
  back.addEventListener("click", openOptions);
  filter.addEventListener("input", renderTaskList);
  document.addEventListener("pointerdown", (event) => {
    if (!menu.hidden && !menu.contains(event.target) && event.target !== trigger) close();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !menu.hidden) close();
  });
  renderTray();

  return {
    warm,
    consume() {
      const pending = selected.find((task) => task.state === "loading");
      if (pending) {
        notify("正在准备选中的 Codex 对话");
        return null;
      }
      const failed = selected.find((task) => task.state === "failed");
      if (failed) {
        notify(failed.error || "无法读取选中的 Codex 对话");
        return null;
      }
      const taskIds = selected.map((task) => task.id);
      selected = [];
      renderTray();
      return taskIds;
    },
    configUpdated() {
      if (!taskAccessEnabled() && !picker.hidden) openOptions();
    },
  };

  function taskAccessEnabled() {
    return state.bridge?.codexTaskAccess === true;
  }

  function openOptions() {
    menu.hidden = false;
    picker.hidden = true;
    options.hidden = false;
    trigger.setAttribute("aria-expanded", "true");
  }

  function close() {
    menu.hidden = true;
    trigger.setAttribute("aria-expanded", "false");
  }

  async function openPicker() {
    if (!taskAccessEnabled()) {
      close();
      openSettings("bridge");
      notify("请先开启 Codex 对话访问");
      return;
    }
    options.hidden = true;
    picker.hidden = false;
    filter.value = "";
    status.textContent = tasks.length ? "最近使用" : "正在读取最近对话";
    renderTaskList();
    filter.focus();
    try {
      await loadTasks();
    } catch (error) {
      status.textContent = error.message;
      renderTaskList();
    }
  }

  async function warm() {
    if (!taskAccessEnabled() || loading) return;
    try {
      await refreshTasks();
    } catch {
      // The picker keeps the interaction-local error visible if the user opens it.
    }
  }

  async function loadTasks() {
    if (tasks.length) {
      renderTaskList();
      return tasks;
    }
    return refreshTasks();
  }

  async function refreshTasks() {
    if (loading) return loading;
    loading = fetch("/api/codex/tasks?refresh=true")
      .then((response) => responseJson(response, "无法读取最近 Codex 对话"))
      .then((value) => {
        tasks = Array.isArray(value) ? value : [];
        saveSessionTasks(tasks);
        status.textContent = tasks.length ? `${tasks.length} 个最近对话` : "没有可见的最近对话";
        renderTaskList();
        return tasks;
      })
      .finally(() => {
        loading = null;
      });
    return loading;
  }

  function renderTaskList() {
    list.replaceChildren();
    const query = filter.value.trim().toLocaleLowerCase();
    const visible = tasks.filter((task) => {
      const searchable = [task.title, task.cwd, task.preview].join(" ").toLocaleLowerCase();
      return !query || searchable.includes(query);
    });
    if (!visible.length) {
      const empty = document.createElement("p");
      empty.textContent = tasks.length ? "没有匹配的对话" : "正在准备最近对话…";
      list.append(empty);
      return;
    }
    for (const task of visible) {
      const item = document.createElement("button");
      item.type = "button";
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", String(selected.some((value) => value.id === task.id)));
      const title = document.createElement("strong");
      title.textContent = task.title || "未命名 Codex 对话";
      const meta = document.createElement("small");
      meta.textContent = [shortPath(task.cwd), formatTaskTime(task.updatedAt)]
        .filter(Boolean)
        .join(" · ");
      item.append(title, meta);
      item.addEventListener("click", () => selectTask(task));
      list.append(item);
    }
  }

  function selectTask(task) {
    if (selected.some((value) => value.id === task.id)) {
      close();
      return;
    }
    if (selected.length >= MAX_CODEX_CONTEXTS) {
      notify(`每轮最多加入 ${MAX_CODEX_CONTEXTS} 个 Codex 对话`);
      return;
    }
    const source = {
      id: task.id,
      title: task.title || "未命名 Codex 对话",
      cwd: task.cwd || "",
      state: "loading",
      error: null,
    };
    selected.push(source);
    renderTray();
    close();
    prepareTask(source);
  }

  async function prepareTask(source) {
    try {
      await responseJson(
        await fetch(`/api/codex/tasks/${encodeURIComponent(source.id)}`),
        "无法读取 Codex 对话",
      );
      source.state = "ready";
    } catch (error) {
      source.state = "failed";
      source.error = error.message;
      notify(error.message);
    }
    renderTray();
  }

  function renderTray() {
    tray.replaceChildren();
    tray.hidden = selected.length === 0;
    for (const source of selected) {
      const chip = document.createElement("div");
      chip.className = "codex-context-chip";
      const title = document.createElement("strong");
      title.textContent = `Codex · ${source.title}`;
      const state = document.createElement("small");
      state.textContent = {
        loading: "准备中",
        ready: "本轮上下文",
        failed: "读取失败",
      }[source.state];
      const remove = document.createElement("button");
      remove.type = "button";
      remove.textContent = "×";
      remove.setAttribute("aria-label", `移除 ${source.title}`);
      remove.addEventListener("click", () => {
        selected = selected.filter((value) => value !== source);
        renderTray();
      });
      chip.append(title, state, remove);
      tray.append(chip);
    }
  }
}

function loadSessionTasks() {
  try {
    const value = JSON.parse(sessionStorage.getItem(SESSION_CACHE_KEY) || "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function saveSessionTasks(tasks) {
  try {
    sessionStorage.setItem(SESSION_CACHE_KEY, JSON.stringify(tasks));
  } catch {
    // The server cache remains available when browser storage is unavailable.
  }
}

function shortPath(path) {
  if (!path) return "";
  return path.split("/").filter(Boolean).slice(-2).join("/");
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
