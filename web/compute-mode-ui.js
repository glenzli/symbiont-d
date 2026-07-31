const MODE_LABELS = {
  auto: "自动",
  investigate: "深入",
  critical: "关键",
};

export function initComputeModeUi() {
  const select = document.querySelector("#compute-mode");
  const trigger = document.querySelector("#compute-mode-trigger");
  const label = document.querySelector("#compute-mode-label");
  const menu = document.querySelector("#compute-mode-menu");
  const options = [...menu.querySelectorAll("[data-compute-mode-option]")];

  function close() {
    menu.hidden = true;
    trigger.setAttribute("aria-expanded", "false");
  }

  function render() {
    const mode = select.value;
    const modeLabel = MODE_LABELS[mode] || MODE_LABELS.auto;
    label.textContent = modeLabel;
    trigger.title = `本轮计算级别：${modeLabel}`;
    trigger.setAttribute("aria-label", `本轮计算级别：${modeLabel}`);
    options.forEach((option) => {
      const selected = option.dataset.computeModeOption === mode;
      option.dataset.selected = String(selected);
      option.setAttribute("aria-pressed", String(selected));
    });
  }

  function choose(mode) {
    if (!MODE_LABELS[mode]) return;
    select.value = mode;
    select.dispatchEvent(new Event("change"));
    close();
  }

  trigger.addEventListener("click", () => {
    const opening = menu.hidden;
    menu.hidden = !opening;
    trigger.setAttribute("aria-expanded", String(opening));
    if (opening) {
      menu.querySelector(`[data-compute-mode-option="${select.value}"]`)?.focus();
    }
  });
  menu.addEventListener("click", (event) => {
    const option = event.target.closest("[data-compute-mode-option]");
    if (option) choose(option.dataset.computeModeOption);
  });
  document.addEventListener("click", (event) => {
    if (!menu.hidden && !event.target.closest?.(".compute-mode-picker")) {
      close();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !menu.hidden) {
      close();
      trigger.focus();
    }
  });
  select.addEventListener("change", render);
  render();

  return {
    reset() {
      select.value = "auto";
      select.dispatchEvent(new Event("change"));
      close();
    },
  };
}
