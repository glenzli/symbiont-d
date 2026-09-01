const SELECTOR = "details[data-signal-popover]";
const controllers = new WeakMap();

// One lifecycle across conversation and briefing: exclusive open state, light
// dismiss, keyboard focus, and viewport-bound placement. No evidence is moved.
export function initSignalPopovers(doc) {
  if (controllers.has(doc)) return controllers.get(doc);
  const view = doc.defaultView;
  let active = null;
  function close(details, restoreFocus = false) {
    if (!details) return;
    details.open = false;
    if (active === details) active = null;
    if (restoreFocus && details.isConnected) details.querySelector(":scope > summary")?.focus();
  }
  function closeAll(except = null) {
    for (const details of doc.querySelectorAll(`${SELECTOR}[open]`)) {
      if (details !== except) close(details);
    }
  }
  function place(details) {
    const panel = details.querySelector(":scope > .signal-popover-panel");
    const summary = details.querySelector(":scope > summary");
    if (!panel || !summary) return;
    const margin = 12;
    const gap = 6;
    const viewport = view.visualViewport;
    const width = viewport?.width || view.innerWidth;
    const height = viewport?.height || view.innerHeight;
    const x = viewport?.offsetLeft || 0;
    const y = viewport?.offsetTop || 0;
    const anchor = summary.getBoundingClientRect();
    const below = Math.max(0, y + height - anchor.bottom - gap - margin);
    const above = Math.max(0, anchor.top - y - gap - margin);
    const upward = below < 240 && above > below;
    panel.style.width = `${Math.max(0, Math.min(560, width - margin * 2))}px`;
    panel.style.maxHeight = `${Math.max(0, Math.min(480, upward ? above : below))}px`;
    const bounds = panel.getBoundingClientRect();
    panel.style.left = `${Math.max(x + margin, Math.min(anchor.left, x + width - bounds.width - margin))}px`;
    panel.style.top = `${Math.max(y + margin, upward ? anchor.top - bounds.height - gap : anchor.bottom + gap)}px`;
  }
  function show(details) {
    closeAll(details);
    active = details;
    details.open = true;
    place(details);
  }
  function click(event) {
    const target = event.target?.closest ? event.target : event.target?.parentElement;
    const summary = target?.closest("summary");
    const details = summary?.parentElement;
    if (details?.matches(SELECTOR) && !target.closest("a,button,input,select,textarea")) {
      event.preventDefault();
      if (details.open) close(details);
      else show(details);
    } else if (!active?.contains(event.target)) {
      closeAll();
    }
  }
  function toggle(event) {
    if (event.target.matches?.(SELECTOR) && event.target.open) show(event.target);
  }
  function keydown(event) {
    if (event.key !== "Escape" || !active?.open) return;
    event.preventDefault();
    event.stopPropagation();
    close(active, true);
  }
  function scroll(event) {
    // Keep scrolling/selection inside the evidence panel usable. Close if its
    // anchor moves with the conversation, rather than leaving a detached popup.
    const panel = active?.querySelector(":scope > .signal-popover-panel");
    if (panel && event.target !== panel && !panel.contains(event.target)) closeAll();
  }
  function resize() { closeAll(); }
  doc.addEventListener("click", click, true);
  doc.addEventListener("toggle", toggle, true);
  doc.addEventListener("keydown", keydown, true);
  doc.addEventListener("scroll", scroll, true);
  view.addEventListener("resize", resize);
  view.visualViewport?.addEventListener("resize", resize);
  const controller = {
    close: closeAll,
    destroy() {
      closeAll();
      doc.removeEventListener("click", click, true);
      doc.removeEventListener("toggle", toggle, true);
      doc.removeEventListener("keydown", keydown, true);
      doc.removeEventListener("scroll", scroll, true);
      view.removeEventListener("resize", resize);
      view.visualViewport?.removeEventListener("resize", resize);
      controllers.delete(doc);
    },
  };
  controllers.set(doc, controller);
  return controller;
}
