// Owns unsaved form drafts and the one in-flight settings save. Domain modules
// still own serialization, persistence and rendering of their settings.
export function initSettingsSession({ dialog, button, status, currentPage, persist, mutationSelector = "" }) {
  const selector = '[data-source-settings-panel], [data-settings-panel]:not([data-settings-panel="sources"])';
  const baselines = new Map();
  const dirty = new Set();
  const notices = new Map();
  let busy = false;

  function fingerprint(page) {
    return JSON.stringify([...page.querySelectorAll("input, select, textarea, button[aria-pressed]")]
      .filter((field) => field.type !== "file")
      .map((field) => [field.value, field.checked, field.getAttribute("aria-pressed")]));
  }

  function captureClean() {
    for (const page of dialog.querySelectorAll(selector)) {
      if (!dirty.has(page)) baselines.set(page, fingerprint(page));
    }
  }

  function isDirty(page) {
    return [...dirty].some((item) => page === item || page.contains(item));
  }

  function refresh() {
    for (const page of dialog.querySelectorAll("[data-settings-panel], [data-source-settings-panel]")) {
      page.dataset.settingsDirty = String(isDirty(page));
    }
    for (const tab of dialog.querySelectorAll("[data-settings-tab], [data-source-settings-tab]")) {
      const name = tab.dataset.sourceSettingsTab || tab.dataset.settingsTab;
      const attr = tab.dataset.sourceSettingsTab ? "data-source-settings-panel" : "data-settings-panel";
      const changed = isDirty(dialog.querySelector(`[${attr}="${name}"]`));
      tab.dataset.dirty = String(changed);
      tab.setAttribute("aria-label", `${tab.textContent.trim()}${changed ? "，有未保存的更改" : ""}`);
    }
    const page = currentPage();
    const otherCount = dirty.size - (dirty.has(page) ? 1 : 0);
    status.textContent = busy ? "保存中…" : [
      notices.get(page) || (dirty.has(page) ? "当前页有未保存更改（关闭后保留）" : ""),
      otherCount ? `另有 ${otherCount} 页未保存` : "",
    ].filter(Boolean).join(" · ");
  }

  function changed(target) {
    if (busy) return;
    const page = target.closest(selector) || currentPage();
    if (!page) return;
    if (fingerprint(page) === baselines.get(page)) dirty.delete(page);
    else dirty.add(page);
    notices.delete(page);
    refresh();
  }

  async function save(event) {
    event?.preventDefault();
    if (busy) return false;
    const page = currentPage();
    // The common save button is outside the forms; it must run the same native
    // constraints as Enter would, including controls in a section rather than a form.
    const invalid = [...page.querySelectorAll("input, select, textarea")].find((field) =>
      !field.closest("[hidden]") && field.willValidate && !field.checkValidity());
    if (invalid) {
      notices.set(page, "请检查标出的输入，尚未保存");
      refresh();
      invalid.reportValidity();
      invalid.focus();
      return false;
    }
    busy = true;
    const controls = [...dialog.querySelectorAll("input, select, textarea, button")]
      .filter((field) => !field.matches("[data-close]"));
    const disabled = controls.map((field) => field.disabled);
    controls.forEach((field) => { field.disabled = true; });
    dialog.setAttribute("aria-busy", "true");
    refresh();
    try {
      const saved = await persist();
      if (saved === false) {
        notices.set(page, status.textContent === "保存中…" ? "保存失败，请重试" : status.textContent);
        return false;
      }
      dirty.delete(page);
      baselines.set(page, fingerprint(page));
      notices.set(page, "已保存");
      return true;
    } catch (error) {
      notices.set(page, error.message || "保存失败，请重试");
      return false;
    } finally {
      busy = false;
      controls.forEach((field, index) => { field.disabled = disabled[index]; });
      button.disabled = false;
      dialog.removeAttribute("aria-busy");
      refresh();
    }
  }

  dialog.addEventListener("input", (event) => changed(event.target));
  dialog.addEventListener("change", (event) => changed(event.target));
  dialog.addEventListener("click", (event) => {
    if (mutationSelector && event.target.closest(mutationSelector)) changed(event.target);
  });
  dialog.addEventListener("submit", save);
  dialog.addEventListener("keydown", (event) => {
    // Text controls outside forms (identity and role names) share the footer's
    // validation and in-flight guard instead of issuing their own partial save.
    if (event.key !== "Enter" || event.target.closest("form")
      || !event.target.matches('input[type="text"], input[type="number"]')) return;
    event.preventDefault();
    event.stopPropagation();
    void save();
  }, true);
  button.addEventListener("click", save);
  dialog.ownerDocument.defaultView.addEventListener("beforeunload", (event) => {
    if (!dirty.size && !busy) return;
    event.preventDefault();
    event.returnValue = "";
  });
  return { captureClean, isDirty, refresh, save, get busy() { return busy; } };
}
