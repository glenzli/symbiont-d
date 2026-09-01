import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";

test("backfilled sources stay new, annotations do not count, and scrolling a tall card clears it", async () => {
  const { window } = new JSDOM('<title>Symbiont</title><button id="unread-indicator"><span id="unread-count"></span></button><main></main>', { url: "http://localhost/", pretendToBeVisual: true });
  Object.assign(globalThis, { window, document: window.document, localStorage: window.localStorage });
  let frames = [];
  globalThis.requestAnimationFrame = fn => frames.push(fn);
  globalThis.IntersectionObserver = class { observe() {} unobserve() {} };
  let focused = false;
  window.document.hasFocus = () => focused;
  localStorage.setItem("symbiont-d:message-state:v1", JSON.stringify({ observedAt: "2026-09-01T00:00:00Z", unreadRevisionIds: ["signal:dissent"] }));
  const { initMessageSync } = await import("./message-sync.js");
  const conversation = document.querySelector("main");
  conversation.getBoundingClientRect = () => ({ top: 0, bottom: 500, height: 500 });
  const sync = initMessageSync({ conversation, appendMessage() {}, applyRuntime() {}, shouldDeferMessages: () => false });
  function article() {
    const element = document.createElement("article");
    element.className = "message";
    element.innerHTML = '<header class="message-meta"></header>';
    element.getClientRects = () => [{}];
    element.getBoundingClientRect = () => ({ top: 900, bottom: 2900, height: 2000 });
    conversation.append(element);
    return element;
  }
  const source = article();
  sync.trackSignal(source, { id: "backfill", observedAt: "2026-09-02T09:00:00Z", sourceDocumentAt: "2026-08-19T12:00:00Z" }, { history: true });
  // Bootstrapping out of timestamp order must still compare against the same
  // persisted last-visit boundary, not a moving maximum.
  const earlier = article();
  sync.trackSignal(earlier, { id: "earlier", observedAt: "2026-09-02T08:00:00Z" }, { history: true });
  sync.trackSignal(article(), { id: "dissent", kind: "attacker_challenge" }, { incoming: true });
  sync.completeBootstrap([], []);
  assert.equal(document.querySelector("#unread-count").textContent, "2");
  assert.equal(document.querySelectorAll(".message.unread").length, 2);
  function flush() { while (frames.length) { const pending = frames; frames = []; pending.forEach(fn => fn()); } }
  focused = true;
  flush();
  assert.equal(document.querySelector("#unread-count").textContent, "2");
  source.getBoundingClientRect = () => ({ top: -50, bottom: 1950, height: 2000 });
  // Collapsed/hidden cards must not count as having been read.
  earlier.getClientRects = () => [];
  earlier.getBoundingClientRect = () => ({ top: 0, bottom: 0, height: 0 });
  conversation.dispatchEvent(new window.Event("scroll"));
  flush();
  assert.equal(document.querySelector("#unread-count").textContent, "1");
  assert.deepEqual(JSON.parse(localStorage.getItem("symbiont-d:message-state:v1")).unreadRevisionIds, ["signal:earlier"]);
  const replacement = article();
  sync.trackSignal(replacement, { id: "backfill", observedAt: "2026-09-02T09:00:00Z" }, { incoming: true, previousElement: source });
  assert.equal(replacement.classList.contains("unread"), false);
  assert.equal(document.querySelector("#unread-count").textContent, "1");
  window.close();
});
