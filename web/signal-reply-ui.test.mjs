import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { JSDOM, VirtualConsole } from "jsdom";

import { initSignalReplyUi } from "./signal-reply-ui.js";

const web = path.dirname(fileURLToPath(import.meta.url));
const signal = (id, name = "Luna") => ({
  id, kind: "external_input", actor: { id: "luna", name },
  title: `来源 ${id}`, content: `完整输入 ${id}`, observedAt: new Date().toISOString(),
});

function fixture(t) {
  const dom = new JSDOM('<div id="signal-reply-tray" hidden></div><textarea id="message"></textarea>');
  const previous = globalThis.document;
  globalThis.document = dom.window.document;
  t.after(() => { globalThis.document = previous; dom.window.close(); });
  const notices = [];
  const input = document.querySelector("textarea");
  const ui = initSignalReplyUi({ focusComposer: () => input.focus(), notify: (text) => notices.push(text) });
  return { ui, notices, input, tray: document.querySelector("#signal-reply-tray") };
}

test("reply selection is visible, replaces its target and cancels without changing typed text", (t) => {
  const { ui, tray, input } = fixture(t);
  input.value = "我的草稿";
  ui.select(signal("a"));
  assert.equal(tray.hidden, false);
  assert.match(tray.textContent, /正在回复 · Luna/);
  assert.match(tray.textContent, /来源 a.*完整输入 a/);
  assert.equal(document.activeElement, input);
  ui.select(signal("b", "Gemini"));
  assert.equal(tray.children.length, 1);
  assert.equal(tray.firstElementChild.dataset.signalId, "b");
  tray.querySelector('[aria-label="取消回复"]').click();
  assert.equal(tray.hidden, true);
  assert.equal(ui.consume(), null);
  assert.equal(input.value, "我的草稿");
});

test("empty submit explains what is missing and keeps the reply selected", (t) => {
  const { ui, tray, notices } = fixture(t);
  ui.select(signal("a"));
  assert.equal(ui.validateSubmission(false), false);
  assert.equal(notices.length, 1);
  assert.match(notices[0], /请填写.*回复对象已保留/);
  assert.equal(tray.hidden, false);
  assert.equal(ui.validateSubmission(true), true);
  assert.equal(ui.consume().signal.id, "a");
  assert.equal(tray.hidden, true);
  assert.equal(ui.consume(), null, "a reply must not leak into the next send");
});

test("failed submission restores its draft but cannot overwrite later user intent", (t) => {
  const { ui, tray } = fixture(t);
  ui.select(signal("a"));
  const failed = ui.consume();
  ui.restore(failed);
  assert.equal(tray.firstElementChild.dataset.signalId, "a");
  const earlier = ui.consume();
  ui.select(signal("b"));
  ui.restore(earlier);
  assert.equal(tray.firstElementChild.dataset.signalId, "b");
  ui.clear();
  ui.restore(earlier);
  assert.equal(tray.hidden, true, "cancelled selection must not reappear");
  ui.select(signal("a"));
  const sameId = ui.consume();
  ui.select(signal("a"));
  ui.clear();
  ui.restore(sameId);
  assert.equal(tray.hidden, true, "identity alone must not bypass the draft generation");
});

test("dismissal targets only the selected source; untrusted previews stay plain text", (t) => {
  const { ui, tray } = fixture(t);
  ui.select({ ...signal("a"), title: '<img src=x onerror="alert(1)">',
    content: "旧摘要", presentation: "condensed", receivedText: "原文细节 ".repeat(100) });
  assert.equal(tray.querySelector("img"), null);
  assert.match(tray.querySelector("p").textContent, /^原文细节/);
  assert.ok(tray.querySelector("p").textContent.length <= 240);
  ui.clear("b");
  assert.equal(tray.hidden, false);
  ui.clear("a");
  assert.equal(tray.hidden, true);
  ui.select(signal("a"));
  const pending = ui.consume();
  ui.clear("b");
  ui.restore(pending);
  assert.equal(tray.hidden, false, "unrelated dismissal must not cancel failure recovery");
  const removed = ui.consume();
  ui.clear("a");
  ui.restore(removed);
  assert.equal(tray.hidden, true, "a source removed during dispatch must never be restored");
});

let applicationBundle;
async function appFixture(t) {
  applicationBundle ||= build({ entryPoints: [path.join(web, "app.js")], bundle: true,
    write: false, format: "iife", logLevel: "silent", plugins: [{ name: "served-modules", setup(builder) {
      builder.onResolve({ filter: /^\// }, ({ path: url, kind }) =>
        kind === "entry-point" ? undefined : { path: path.join(web, url) });
    } }] });
  const errors = [];
  const console = new VirtualConsole();
  console.on("jsdomError", (error) => errors.push(error.message));
  const dom = new JSDOM(await readFile(path.join(web, "index.html"), "utf8"), {
    url: "http://symbiont.test", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console,
  });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.HTMLCanvasElement.prototype.getContext = () => null;
  window.HTMLElement.prototype.scrollIntoView = () => {};
  window.HTMLElement.prototype.scrollTo = () => {};
  window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  window.HTMLDialogElement.prototype.close = function () { this.open = false; };
  window.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
  window.EventSource = class { addEventListener() {} close() {} };
  window.TextDecoder = TextDecoder;
  const requests = [];
  let chatHandler = () => new Response(JSON.stringify({ error: "测试拒绝" }), { status: 503 });
  const state = {
    messages: [], signals: [signal("a"), signal("b")], historyHasMore: false, memoryChars: 0,
    profile: { status: "ready" }, inputRoles: { roles: [{ id: "luna", name: "Luna" }], avatarOptions: [] },
  };
  window.fetch = async (url, options = {}) => {
    requests.push({ url, method: options.method || "GET", body: options.body });
    if (url === "/api/chat" || url === "/api/chat/append") return chatHandler(url, options);
    if (url === "/api/bootstrap") return Response.json(state);
    if (url.startsWith("/api/runtime")) return Response.json({ messages: [], connection: "ready" });
    if (url.startsWith("/api/model-council/activation")) return Response.json({ participantIds: [] });
    if (url.startsWith("/api/temporary-discussion")) return Response.json({ active: false, turns: [] });
    throw new Error(`Unexpected test request: ${url}`);
  };
  window.eval((await applicationBundle).outputFiles[0].text);
  const flush = async () => { for (let i = 0; i < 12; i += 1) await new Promise((resolve) => setImmediate(resolve)); };
  await flush();
  assert.deepEqual(errors, []);
  assert.equal(window.document.querySelector("#composer").hidden, false,
    window.document.querySelector("#conversation").textContent);
  return {
    doc: window.document, requests, flush,
    reply: (id) => window.document.querySelector(`[data-signal-id="${id}"] .input-signal-reply`).click(),
    submit(text = "") {
      window.document.querySelector("#message").value = text;
      window.document.querySelector("#composer").dispatchEvent(new window.Event("submit", { bubbles: true, cancelable: true }));
    },
    setChatHandler: (handler) => { chatHandler = handler; },
  };
}

test("real app reply buttons, empty-submit guard and failed-request restoration are wired together", async (t) => {
  const app = await appFixture(t);
  const tray = app.doc.querySelector("#signal-reply-tray");
  app.reply("a");
  assert.equal(tray.hidden, false);
  app.submit();
  await app.flush();
  assert.equal(app.requests.filter(({ url }) => url === "/api/chat").length, 0);
  assert.match(app.doc.querySelector("#app-status-message").textContent, /请填写/);
  app.reply("b");
  app.submit("讨论这条");
  await app.flush();
  const request = app.requests.find(({ url }) => url === "/api/chat");
  assert.equal(request.body.get("signalId"), "b");
  assert.equal(request.body.get("message"), "讨论这条");
  assert.equal(tray.firstElementChild.dataset.signalId, "b", "failed dispatch restores the target");
  tray.querySelector("button").click();
  app.submit("另一个问题");
  await app.flush();
  assert.equal(app.requests.filter(({ url }) => url === "/api/chat").at(-1).body.get("signalId"), null);
});

test("briefing uses the same reply card and successful handoff clears only the submitted draft", async (t) => {
  const app = await appFixture(t);
  app.doc.querySelector("#open-input-briefing").click();
  app.doc.querySelector(".input-briefing-reply").click();
  const tray = app.doc.querySelector("#signal-reply-tray");
  assert.equal(app.doc.querySelector("#input-briefing-dialog").open, false);
  assert.equal(tray.firstElementChild.dataset.signalId, "a");
  let finish;
  app.setChatHandler(() => new Promise((resolve) => { finish = resolve; }));
  app.submit("讨论 a");
  assert.equal(tray.hidden, true);
  app.reply("b");
  finish(new Response(`${JSON.stringify({ type: "interrupted" })}\n`));
  await app.flush();
  assert.equal(tray.firstElementChild.dataset.signalId, "b", "finishing an older turn must preserve the next reply");
});

test("retry preserves the failed source without borrowing or clearing a different reply draft", async (t) => {
  const app = await appFixture(t);
  app.reply("a");
  app.submit("讨论 a");
  await app.flush();
  app.reply("b");
  app.setChatHandler(() => new Response(`${JSON.stringify({ type: "interrupted" })}\n`));
  app.doc.querySelector('[data-message-action="retry"]').click();
  await app.flush();
  const requests = app.requests.filter(({ url }) => url === "/api/chat");
  assert.equal(requests.at(-1).body.get("signalId"), "a");
  assert.equal(app.doc.querySelector(".composer-signal-reply").dataset.signalId, "b");
});

test("append failure preserves its reply while the original response is still active", async (t) => {
  const app = await appFixture(t);
  let finish;
  app.setChatHandler((url) => url === "/api/chat"
    ? new Promise((resolve) => { finish = resolve; })
    : new Response(JSON.stringify({ error: "追加被拒绝" }), { status: 503 }));
  app.submit("普通对话");
  app.reply("b");
  app.submit("补充回应 b");
  await app.flush();
  assert.equal(app.requests.find(({ url }) => url === "/api/chat/append").body.get("signalId"), "b");
  assert.equal(app.doc.querySelector(".composer-signal-reply").dataset.signalId, "b");
  finish(new Response(`${JSON.stringify({ type: "interrupted" })}\n`));
  await app.flush();
  assert.equal(app.doc.querySelector(".composer-signal-reply").dataset.signalId, "b");
});
