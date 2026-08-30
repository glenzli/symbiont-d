export function initModelCouncilUi({ state, notify }) {
  const picker = document.querySelector(".model-council-picker");
  const toggle = document.querySelector("#model-council-toggle");
  const menu = document.querySelector("#model-council-menu");
  const options = document.querySelector("#model-council-options");
  const status = document.querySelector("#model-council-status");
  const count = document.querySelector("#model-council-count");
  const selected = new Set();

  function available() {
    return (state.modelCouncil?.participants || []).filter((item) => item.enabled);
  }

  function render() {
    const participants = available();
    for (const id of [...selected]) {
      if (!participants.some((item) => item.id === id)) selected.delete(id);
    }
    options.replaceChildren();
    if (!participants.length) {
      const empty = document.createElement("p");
      empty.textContent = "尚未配置可用的潜水模型。";
      options.append(empty);
    }
    for (const participant of participants) {
      const label = document.createElement("label");
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = selected.has(participant.id);
      input.disabled = !input.checked && selected.size >= maximum();
      input.addEventListener("change", () => {
        if (input.checked && selected.size >= maximum()) {
          input.checked = false;
          notify?.(`每轮最多召集 ${maximum()} 个模型`);
          return;
        }
        if (input.checked) selected.add(participant.id);
        else selected.delete(participant.id);
        render();
      });
      const avatar = document.createElement("span");
      avatar.className = "model-council-option-avatar";
      avatar.textContent = participant.avatar || "◌";
      const text = document.createElement("span");
      const name = document.createElement("strong");
      name.textContent = participant.name;
      const detail = document.createElement("small");
      detail.textContent = participant.role || participant.model;
      text.append(name, detail);
      label.append(input, avatar, text);
      options.append(label);
    }
    const size = selected.size;
    count.hidden = size === 0;
    count.textContent = String(size);
    toggle.classList.toggle("active", size > 0);
    status.textContent = size
      ? `本轮将并行召集 ${size} 个模型，发送后恢复潜水。`
      : "默认潜水，不产生调用。";
  }

  function maximum() {
    return state.modelCouncil?.maximumSelected || 3;
  }

  function close() {
    menu.hidden = true;
    toggle.setAttribute("aria-expanded", "false");
  }

  toggle.addEventListener("click", () => {
    const opening = menu.hidden;
    menu.hidden = !opening;
    toggle.setAttribute("aria-expanded", String(opening));
    if (opening) render();
  });
  document.addEventListener("click", (event) => {
    if (!picker.contains(event.target)) close();
  });
  window.addEventListener("symbiont:model-council-updated", render);

  return {
    configUpdated: render,
    consume() {
      const ids = [...selected];
      selected.clear();
      close();
      render();
      return ids;
    },
  };
}
