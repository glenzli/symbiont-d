import { responseJson } from "./presentation.js";

const labels = {
  paused: "已暂停",
  waiting_first_check: "等待首次检查",
  ready: "已记录检查进度",
};

export function initXWatchUi(state, markClean = () => {}) {
  const panel = document.querySelector("#x-watch-form");
  const list = document.querySelector("#x-watch-list");
  const status = document.querySelector("#x-watch-save-state");
  const newHandle = document.querySelector("#x-watch-new-handle");
  const newFocus = document.querySelector("#x-watch-new-focus");
  const newDelivery = document.querySelector("#x-watch-new-delivery");
  const checkAll = document.querySelector("#x-watch-check-all");

  function button(label, action) {
    const element = document.createElement("button");
    element.className = "secondary-button";
    element.type = "button";
    element.textContent = label;
    element.addEventListener("click", action);
    return element;
  }

  function field(label, value) {
    const wrapper = document.createElement("label");
    wrapper.className = "field";
    const caption = document.createElement("span");
    caption.textContent = label;
    const input = document.createElement("input");
    input.value = value ?? "";
    wrapper.append(caption, input);
    return { wrapper, input };
  }

  async function check(key = null) {
    if (hasDrafts() && !await saveAll()) return;
    panel.dispatchEvent(new CustomEvent("symbiont:x-watch-check", {
      bubbles: true, detail: { key },
    }));
  }

  function render({ preserveDrafts = false, savedKey = null } = {}) {
    const snapshot = state.xWatches;
    if (!snapshot) return;
    const drafts = new Map(preserveDrafts ? [...list.querySelectorAll(".x-watch-row")].map((row) => [
      row.dataset.watchId,
      { focus: row.querySelector(".x-watch-focus").value,
        delivery: row.querySelector(".x-watch-delivery").value },
    ]) : []);
    list.replaceChildren();
    checkAll.disabled = !snapshot.watches?.some((watch) => watch.enabled);
    if (!snapshot.watches?.length) {
      const empty = document.createElement("p");
      empty.className = "ambient-empty-state";
      empty.textContent = "还没有关注的 X 账号。";
      list.append(empty);
      return;
    }
    for (const watch of snapshot.watches) {
      const row = document.createElement("div");
      row.className = "ambient-config-row x-watch-row";
      row.dataset.watchId = watch.id;
      const heading = document.createElement("div");
      heading.className = "ambient-config-row-head";
      const title = document.createElement("strong");
      title.textContent = `@${watch.handle}`;
      const badge = document.createElement("span");
      badge.className = "ambient-row-status";
      badge.textContent = labels[watch.status] || watch.status;
      heading.append(title, badge);
      const grid = document.createElement("div");
      grid.className = "field-grid";
      const focus = field("关注点", watch.focus);
      focus.input.className = "x-watch-focus";
      focus.input.maxLength = 300;
      const delivery = document.createElement("label");
      delivery.className = "field";
      const deliveryCaption = document.createElement("span");
      deliveryCaption.textContent = "呈现方式";
      const select = document.createElement("select");
      select.className = "x-watch-delivery";
      for (const [value, label] of [["digest", "合并摘要"], ["important", "只看重要变化"]]) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = label;
        select.append(option);
      }
      select.value = watch.delivery;
      if (drafts.has(watch.id) && savedKey !== watch.id) {
        focus.input.value = drafts.get(watch.id).focus;
        select.value = drafts.get(watch.id).delivery;
      }
      delivery.append(deliveryCaption, select);
      grid.append(focus.wrapper, delivery);
      const note = document.createElement("small");
      note.className = "ambient-credential-note source-channel-note";
      note.textContent = `上次检查：${watch.lastCheckedAt ? new Date(watch.lastCheckedAt).toLocaleString() : "尚未检查"}；上次发现 ${watch.lastNewCount || 0} 条新帖。`;
      const controls = document.createElement("div");
      controls.className = "x-watch-controls";
      controls.append(
        button("保存规则", () => void command({ action: "update", key: watch.id,
          focus: focus.input.value.trim(), delivery: select.value })),
        button("检查动态", () => void check(watch.id)),
        button(watch.enabled ? "暂停" : "恢复", () => void command({ action: watch.enabled ? "pause" : "resume", key: watch.id })),
        button("移除", () => {
          if (window.confirm(`移除对 @${watch.handle} 的关注及其检查进度？`)) {
            void command({ action: "remove", key: watch.id });
          }
        }),
      );
      controls.children[1].disabled = !watch.enabled;
      row.append(heading, grid, note, controls);
      if (watch.recentPosts?.length) {
        const recent = document.createElement("div");
        recent.className = "x-watch-recent";
        const label = document.createElement("small");
        label.textContent = "上次浏览器检查记录";
        recent.append(label);
        for (const post of watch.recentPosts.slice(0, 3)) {
          const link = document.createElement("a");
          link.href = post.url;
          link.target = "_blank";
          link.rel = "noopener noreferrer";
          link.textContent = `${post.postedAt ? new Date(post.postedAt).toLocaleString() + " · " : ""}${post.text.slice(0, 130)}`;
          recent.append(link);
        }
        row.append(recent);
      }
      list.append(row);
    }
  }

  async function command(payload) {
    status.textContent = "正在保存关注项…";
    try {
      state.xWatches = await responseJson(await fetch("/api/x-watches", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify(payload),
      }));
      render({ preserveDrafts: true, savedKey: payload.action === "update" ? payload.key : null });
      if (!hasDrafts()) markClean();
      status.textContent = "关注项已更新";
      return true;
    } catch (error) {
      status.textContent = error.message;
      return false;
    }
  }

  function pendingUpdates() {
    return [...list.querySelectorAll(".x-watch-row")].flatMap((row) => {
      const watch = state.xWatches?.watches.find((item) => item.id === row.dataset.watchId);
      if (!watch) return [];
      const focus = row.querySelector(".x-watch-focus").value.trim();
      const delivery = row.querySelector(".x-watch-delivery").value;
      if (focus === watch.focus && delivery === watch.delivery) return [];
      return [{ action: "update", key: watch.id, focus, delivery }];
    });
  }

  function hasDrafts() {
    return pendingUpdates().length > 0 || Boolean(newHandle.value.trim() || newFocus.value.trim());
  }

  async function saveAll() {
    status.textContent = "正在保存关注设置…";
    try {
      for (const update of pendingUpdates()) {
        state.xWatches = await responseJson(await fetch("/api/x-watches", {
          method: "POST", headers: { "content-type": "application/json" },
          body: JSON.stringify(update),
        }));
      }
      if (newHandle.value.trim() || newFocus.value.trim()) {
        state.xWatches = await responseJson(await fetch("/api/x-watches", {
          method: "POST", headers: { "content-type": "application/json" },
          body: JSON.stringify({ action: "add", handle: newHandle.value.trim(),
            focus: newFocus.value.trim(), delivery: newDelivery.value }),
        }));
        newHandle.value = "";
        newFocus.value = "";
      }
      render();
      markClean();
      status.textContent = "已保存";
      return true;
    } catch (error) {
      status.textContent = error.message;
      return false;
    }
  }

  checkAll.addEventListener("click", () => void check());
  document.querySelector("#x-watch-add").addEventListener("click", async () => {
    const saved = await command({ action: "add", handle: newHandle.value.trim(),
      focus: newFocus.value.trim(), delivery: newDelivery.value });
    if (saved) {
      newHandle.value = "";
      newFocus.value = "";
      if (!hasDrafts()) markClean();
    }
  });
  panel.addEventListener("submit", (event) => event.preventDefault());
  return { render, saveAll };
}
