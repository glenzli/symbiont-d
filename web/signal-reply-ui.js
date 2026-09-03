import { signalContent } from "./input-signal-content.js";

// A reply is a one-message draft, not ambient context for later sends.
export function initSignalReplyUi({ focusComposer, notify }) {
  const tray = document.querySelector("#signal-reply-tray");
  let selected = null;
  let generation = 0;
  let restorableSignalId = null;

  function render() {
    tray.replaceChildren();
    tray.hidden = !selected;
    if (!selected) return;
    const card = document.createElement("div");
    card.className = "composer-quote composer-signal-reply";
    card.dataset.signalId = selected.id;
    const content = document.createElement("div");
    const meta = document.createElement("span");
    meta.textContent = `正在回复 · ${selected.actor?.name || "外部输入"}`;
    const title = document.createElement("strong");
    title.textContent = selected.title || "这条外部输入";
    const excerpt = document.createElement("p");
    excerpt.textContent = signalContent(selected).text.replace(/\s+/g, " ").trim().slice(0, 240);
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = "×";
    cancel.title = "取消回复";
    cancel.setAttribute("aria-label", "取消回复");
    cancel.addEventListener("click", () => {
      clear();
      focusComposer();
    });
    content.append(meta, title, excerpt);
    card.append(content, cancel);
    tray.append(card);
  }

  function clear(signalId = null) {
    if (signalId && selected?.id !== signalId && restorableSignalId !== signalId) return;
    generation += 1;
    restorableSignalId = null;
    selected = null;
    render();
  }

  return {
    select(signal) {
      if (!signal?.id) {
        notify("这条输入暂时无法回复，请刷新后重试");
        return;
      }
      generation += 1;
      restorableSignalId = null;
      selected = signal;
      render();
      focusComposer();
    },
    clear,
    validateSubmission(hasContent) {
      if (hasContent) return true;
      if (selected) {
        notify("请填写你想回应的内容，回复对象已保留");
        focusComposer();
      }
      return false;
    },
    consume(signalId = null) {
      if (!selected || (signalId && selected.id !== signalId)) return null;
      const signal = selected;
      clear();
      restorableSignalId = signal.id;
      return { signal, generation };
    },
    restore(draft) {
      // A failed earlier request must not replace a newer selection, even if
      // the user selected the same signal again or subsequently cancelled it.
      if (!draft || selected || draft.generation !== generation) return;
      generation += 1;
      restorableSignalId = null;
      selected = draft.signal;
      render();
    },
  };
}
