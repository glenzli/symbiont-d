import { responseJson } from "/presentation.js";

const DEFAULT_SYMBIONT_AVATAR_URL =
  "/symbiont-avatar.png?v=clay-transparent-20260810-v1";
const DEFAULT_SMALL_SYMBIONT_AVATAR_URL =
  "/symbiont-avatar-small.png?v=clay-transparent-20260810-v1";

export function initIdentityUi(state) {
  const displayNameInput = document.querySelector("#symbiont-display-name");
  const displayNameHeading = document.querySelector("#symbiont-display-name-heading");
  const topbarIdentity = displayNameHeading?.closest(".identity");
  const symbiontAvatarLabel = document.querySelector("#symbiont-avatar-label");
  const symbiontPreview = document.querySelector("#avatar-preview");
  const symbiontChoose = document.querySelector("#choose-avatar");
  const symbiontClear = document.querySelector("#clear-avatar");
  const symbiontInput = document.querySelector("#avatar-input");
  const symbiontSaveState = document.querySelector("#avatar-save-state");
  const userPreview = document.querySelector("#user-avatar-preview");
  const userFallback = document.querySelector("#user-avatar-fallback");
  const userChoose = document.querySelector("#choose-user-avatar");
  const userClear = document.querySelector("#clear-user-avatar");
  const userInput = document.querySelector("#user-avatar-input");
  const userSaveState = document.querySelector("#user-avatar-save-state");

  function displayName() {
    return state.identity?.displayName?.trim() || "symbiont-d";
  }

  function avatar(slot) {
    return slot === "user" ? state.identity?.userAvatar : state.identity?.avatar;
  }

  function applyImage(image, fallback, slot) {
    if (!image) return;
    const url = avatar(slot)?.url;
    if (!url && slot === "user") {
      image.hidden = true;
      if (fallback) fallback.hidden = false;
      return;
    }

    image.hidden = false;
    if (fallback) fallback.hidden = true;
    const defaultUrl = slot === "symbiont"
      ? image.closest(".message-avatar")
        ? DEFAULT_SMALL_SYMBIONT_AVATAR_URL
        : DEFAULT_SYMBIONT_AVATAR_URL
      : null;
    image.onerror = () => {
      if (defaultUrl && !image.src.endsWith(defaultUrl)) {
        image.src = defaultUrl;
        return;
      }
      image.hidden = true;
      if (fallback) fallback.hidden = false;
    };
    image.src = url || defaultUrl;
  }

  function applyAvatar(container, slot) {
    applyImage(
      container?.querySelector(".message-avatar-image"),
      container?.querySelector(".message-avatar-fallback"),
      slot,
    );
  }

  function render() {
    const name = displayName();
    if (displayNameInput && document.activeElement !== displayNameInput) {
      displayNameInput.value = name;
    }
    if (displayNameHeading) displayNameHeading.textContent = name;
    if (topbarIdentity) topbarIdentity.setAttribute("aria-label", name);
    if (symbiontAvatarLabel) symbiontAvatarLabel.textContent = `${name} 头像`;
    if (symbiontPreview) symbiontPreview.alt = `${name} 头像`;
    document.querySelectorAll('.message[data-role="assistant"] .speaker').forEach((speaker) => {
      speaker.textContent = name;
    });
    document.querySelectorAll(".message-avatar").forEach((container) => {
      const role = container.closest("[data-role]")?.dataset.role;
      applyAvatar(container, role === "user" ? "user" : "symbiont");
    });
    applyImage(symbiontPreview, null, "symbiont");
    applyImage(userPreview, userFallback, "user");
    symbiontClear.disabled = !avatar("symbiont");
    userClear.disabled = !avatar("user");
  }

  async function save() {
    const name = displayNameInput?.value.trim() || "";
    try {
      state.identity = await responseJson(
        await fetch("/api/identity", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ displayName: name }),
        }),
        "无法保存昵称",
      );
      render();
      window.dispatchEvent(
        new CustomEvent("identity-updated", { detail: state.identity }),
      );
      return true;
    } catch (error) {
      symbiontSaveState.textContent = error.message;
      return false;
    }
  }

  function controls(slot) {
    return slot === "user"
      ? {
          choose: userChoose,
          clear: userClear,
          input: userInput,
          saveState: userSaveState,
          url: "/api/identity/user-avatar",
        }
      : {
          choose: symbiontChoose,
          clear: symbiontClear,
          input: symbiontInput,
          saveState: symbiontSaveState,
          url: "/api/identity/avatar",
        };
  }

  async function upload(slot) {
    const { choose, clear, input, saveState, url } = controls(slot);
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;

    choose.disabled = true;
    clear.disabled = true;
    saveState.textContent = "正在保存";
    try {
      const body = new FormData();
      body.append("avatar", file, file.name);
      state.identity = await responseJson(
        await fetch(url, { method: "POST", body }),
        "无法保存头像",
      );
      saveState.textContent = "已保存";
      render();
    } catch (error) {
      saveState.textContent = error.message;
    } finally {
      choose.disabled = false;
      clear.disabled = !avatar(slot);
    }
  }

  async function clear(slot) {
    const { choose, clear: clearButton, saveState, url } = controls(slot);
    choose.disabled = true;
    clearButton.disabled = true;
    saveState.textContent = slot === "user" ? "正在清除" : "正在还原";
    try {
      state.identity = await responseJson(
        await fetch(url, { method: "DELETE" }),
        slot === "user" ? "无法清除头像" : "无法还原默认头像",
      );
      saveState.textContent = slot === "user" ? "已清除" : "已还原默认";
      render();
    } catch (error) {
      saveState.textContent = error.message;
    } finally {
      choose.disabled = false;
      clearButton.disabled = !avatar(slot);
    }
  }

  for (const slot of ["symbiont", "user"]) {
    const { choose, clear: clearButton, input } = controls(slot);
    choose.addEventListener("click", () => input.click());
    input.addEventListener("change", () => upload(slot));
    clearButton.addEventListener("click", () => clear(slot));
  }

  return { applyAvatar, displayName, render, save };
}
