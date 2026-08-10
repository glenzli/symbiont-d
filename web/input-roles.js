import { responseJson } from "/presentation.js";

const AVATAR_LABELS = {
  "moon-window": "月窗",
  courier: "信使",
  prism: "棱镜",
  firefly: "萤火",
  tide: "潮汐",
  seed: "种子",
  "star-map": "星图",
  echo: "回声",
};

const LEGACY_AVATARS = {
  orbit: "moon-window",
  ripple: "tide",
  spark: "firefly",
  comet: "courier",
  moss: "seed",
  dawn: "star-map",
};

export function applyInputRoleAvatar(element, avatar = "moon-window") {
  if (!element) return;
  const selected = AVATAR_LABELS[avatar] ? avatar : LEGACY_AVATARS[avatar] || "moon-window";
  element.classList.add("input-role-avatar");
  element.dataset.inputAvatar = selected;
  element.dataset.avatarLoadFailed = "false";
  let image = element.querySelector(":scope > img");
  if (!image) {
    image = document.createElement("img");
    image.alt = "";
    image.decoding = "async";
    image.addEventListener("error", () => {
      element.dataset.avatarLoadFailed = "true";
      image.hidden = true;
    });
    image.addEventListener("load", () => {
      element.dataset.avatarLoadFailed = "false";
      image.hidden = false;
    });
    element.replaceChildren(image);
  }
  image.hidden = false;
  image.src = `/assets/input-role-avatars/${encodeURIComponent(selected)}.png?v=clay-transparent-symmetric-20260810-v1`;
}

export function initInputRoleUi(state) {
  const list = document.querySelector("#input-role-settings-list");
  const empty = document.querySelector("#input-role-settings-empty");
  const template = document.querySelector("#input-role-settings-template");
  const status = document.querySelector("#input-role-settings-state");
  let dirty = false;

  function render({ force = false } = {}) {
    if (!list || !template) return;
    if (dirty && !force) return;
    list.replaceChildren();
    const roles = state.inputRoles?.roles || [];
    empty.hidden = roles.length > 0;
    for (const role of roles) {
      const fragment = template.content.cloneNode(true);
      const row = fragment.querySelector("[data-input-role]");
      row.dataset.roleId = role.id;
      const preview = row.querySelector(".input-role-settings-avatar");
      applyInputRoleAvatar(preview, role.avatar);
      row.querySelector(".input-role-settings-source").textContent = role.source;
      const name = row.querySelector('[data-input-role-field="name"]');
      name.value = role.name;
      name.placeholder = role.defaultName;
      name.setAttribute("aria-label", `${role.source}的昵称`);
      name.addEventListener("input", () => {
        dirty = true;
        status.textContent = "昵称有未保存的更改";
      });
      name.addEventListener("keydown", (event) => {
        if (event.key !== "Enter") return;
        event.preventDefault();
        void saveRoles();
      });
      const choices = row.querySelector(".input-role-avatar-options");
      for (const avatar of state.inputRoles?.avatarOptions || []) {
        const option = document.createElement("button");
        option.type = "button";
        option.className = "input-role-avatar-option";
        option.dataset.avatar = avatar;
        option.title = AVATAR_LABELS[avatar] || avatar;
        option.setAttribute("aria-label", `使用${option.title}头像`);
        option.setAttribute("aria-pressed", String(avatar === role.avatar));
        applyInputRoleAvatar(option, avatar);
        option.addEventListener("click", () => {
          for (const sibling of choices.children) sibling.setAttribute("aria-pressed", "false");
          option.setAttribute("aria-pressed", "true");
          row.dataset.avatar = avatar;
          applyInputRoleAvatar(preview, avatar);
          dirty = true;
          status.textContent = "头像有未保存的更改";
        });
        choices.append(option);
      }
      row.dataset.avatar = role.avatar;
      list.append(fragment);
    }
  }

  async function saveRoles() {
    status.textContent = "保存中…";
    try {
      const roles = [...list.querySelectorAll("[data-input-role]")].map((row) => ({
        id: row.dataset.roleId,
        name: row.querySelector('[data-input-role-field="name"]').value.trim(),
        avatar: row.dataset.avatar,
      }));
      state.inputRoles = await responseJson(
        await fetch("/api/input-roles", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ roles }),
        }),
      );
      dirty = false;
      render({ force: true });
      status.textContent = "已保存";
      window.dispatchEvent(new CustomEvent("input-roles-updated", { detail: state.inputRoles }));
      return true;
    } catch (error) {
      status.textContent = error.message;
      return false;
    }
  }

  async function refresh() {
    if (dirty) return true;
    try {
      state.inputRoles = await responseJson(await fetch("/api/input-roles"));
      render({ force: true });
      return true;
    } catch (error) {
      status.textContent = error.message;
      return false;
    }
  }

  return { render, refresh, save: saveRoles };
}
