import test from "node:test";
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { shouldShowSignal, pruneExpiredSignals } from "./input-signal-history.js";
const now = Date.parse("2026-09-10T12:00:00Z");
const old = { id: "old", kind: "external_input", observedAt: "2026-09-09T11:59:59Z" };
test("timeline expires only unreplied deliveries older than 24h", () => {
  assert.equal(shouldShowSignal(old, [], now), false);
  assert.equal(shouldShowSignal({ ...old, observedAt: "2026-09-09T12:00:00Z" }, [], now), true);
  assert.equal(shouldShowSignal(old, [old.id], now), true);
  assert.equal(shouldShowSignal({ ...old, observedAt: "2026-09-10T12:00:00Z", sourceDocumentAt: "2020-01-01", eventAt: "2020-01-01" }, [], now), true);
  assert.equal(shouldShowSignal({ ...old, observedAt: "unknown" }, [], now), true);
  assert.equal(shouldShowSignal({ ...old, dismissed: true }, [old.id], now), false);
  assert.equal(shouldShowSignal({ ...old, kind: "attacker_challenge" }, [], now), false);
});
test("backward browsing keeps the visible source and reader anchor while removing offscreen expired cards", () => {
  const { window } = new JSDOM('<main><article class="message input-signal" data-signal-id="old"></article><article class="message input-signal" data-signal-id="visible"></article></main>');
  globalThis.document = window.document;
  const conversation = document.querySelector("main");
  const [expired, anchor] = conversation.children;
  conversation.scrollTop = 600;
  conversation.getBoundingClientRect = () => ({ top: 0, bottom: 500 });
  expired.getClientRects = anchor.getClientRects = () => [{}];
  expired.getBoundingClientRect = () => ({ top: -300, bottom: -100 });
  anchor.getBoundingClientRect = () => ({ top: expired.isConnected ? 20 : -180, bottom: 400 });
  const removed = [];
  assert.equal(pruneExpiredSignals({ conversation, signals: [old, { ...old, id: "visible" }], now, onRemove: id => removed.push(id) }), 1);
  assert.deepEqual(removed, ["old"]);
  assert.equal(anchor.isConnected, true);
  assert.equal(conversation.scrollTop, 400);
  window.close();
});

test("archive replies and older message pages stay in chronological order", async () => {
  const { placeTimelineItem } = await import("./input-signal-history.js");
  const { window } = new JSDOM('<main><button>history</button></main>');
  const conversation = window.document.querySelector("main");
  for (const hour of [8, 10, 5, 7, 9]) {
    const article = window.document.createElement("article");
    article.className = "message";
    article.innerHTML = `<time datetime="2026-09-10T${String(hour).padStart(2, "0")}:00:00Z"></time>`;
    article.dataset.hour = hour;
    placeTimelineItem(conversation, article);
  }
  assert.deepEqual([...conversation.querySelectorAll(".message")].map(e => Number(e.dataset.hour)), [5, 7, 8, 9, 10]);
  window.close();
});

test("date archive loads older sources absent from live chat and ignores stale date responses", async () => {
  const { readFile } = await import("node:fs/promises");
  const { initInputBriefingUi } = await import("./input-briefing-ui.js");
  const html = await readFile(new URL("./index.html", import.meta.url), "utf8");
  const { window } = new JSDOM(html, { url: "http://localhost/" });
  Object.assign(globalThis, { document: window.document, window });
  const dialog = document.querySelector("#input-briefing-dialog");
  dialog.showModal = () => dialog.open = true;
  const pending = [];
  globalThis.fetch = url => new Promise(resolve => pending.push({ url, resolve }));
  const state = { signals: [], signalsVersion: 0, inputRoles: { roles: [{ id: "luna", name: "Luna" }] } };
  const ui = initInputBriefingUi({ state, renderMessageContent(element, entry) { element.textContent = entry.content; }, applyAvatar() {}, renderIcons() {}, onReply() {} });
  ui.render();
  assert.equal(document.querySelector("#open-input-briefing").hidden, false);
  document.querySelector("#open-input-briefing").click();
  const date = document.querySelector("#input-briefing-date");
  date.value = "2026-09-01";
  date.dispatchEvent(new window.Event("change"));
  assert.equal(pending.length, 2);
  pending[1].resolve(Response.json([{ ...old, observedAt: "2026-09-01T12:00:00Z", actor: { id: "luna", name: "Luna" }, title: "Archived source", content: "Original text", receivedText: "Original text" }]));
  await new Promise(resolve => setImmediate(resolve));
  assert.match(document.querySelector("#input-briefing-content").textContent, /Archived source/);
  pending[0].resolve(Response.json([]));
  await new Promise(resolve => setImmediate(resolve));
  assert.match(document.querySelector("#input-briefing-content").textContent, /Archived source/);
  assert.equal(state.signals.length, 0, "archive lookup must not repopulate chat");
  window.close();
});
