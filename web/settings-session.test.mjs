import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { readFile } from "node:fs/promises";
import { initSettingsSession } from "./settings-session.js";

function fixture(persist) {
  const dom = new JSDOM(`<dialog open>
    <button data-settings-tab="exploration">探索</button><button data-settings-tab="system">连接</button>
    <section data-settings-panel="exploration"><form><input type="number" min="1" max="3" value="1"></form></section>
    <section data-settings-panel="system" hidden><input type="text" value="zh"></section>
    <span role="status"></span><button id="save">保存当前页</button>
  </dialog>`);
  const doc = dom.window.document;
  const dialog = doc.querySelector("dialog");
  let page = doc.querySelector('[data-settings-panel="exploration"]');
  const status = doc.querySelector('[role="status"]');
  const session = initSettingsSession({ dialog, button: doc.querySelector("#save"), status,
    currentPage: () => page, persist });
  session.captureClean();
  const edit = (value) => {
    const field = page.querySelector("input");
    field.value = value;
    field.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
  };
  const select = (name) => {
    page.hidden = true;
    page = doc.querySelector(`[data-settings-panel="${name}"]`);
    page.hidden = false;
    session.refresh();
  };
  return { dom, doc, dialog, session, edit, select, status, page: () => page };
}

test("footer save validates inputs before any persistence", async () => {
  let calls = 0;
  const f = fixture(async () => { calls++; });
  f.edit("0");
  assert.equal(await f.session.save(), false);
  assert.equal(calls, 0);
  assert.match(f.status.textContent, /尚未保存/);
  assert.equal(f.doc.activeElement, f.page().querySelector("input"));
  f.dom.window.close();
});

test("one save owns buttons and form submits; other page drafts remain unsaved", async () => {
  let finish;
  let calls = 0;
  const f = fixture(() => { calls++; return new Promise((resolve) => { finish = resolve; }); });
  f.edit("2");
  f.select("system");
  f.edit("en");
  f.select("exploration");
  const saving = f.session.save();
  assert.equal(await f.session.save(), false);
  f.page().querySelector("form").dispatchEvent(new f.dom.window.Event("submit", { bubbles: true, cancelable: true }));
  assert.equal(calls, 1);
  assert.equal(f.doc.querySelector('[data-settings-tab="system"]').disabled, true);
  assert.equal(f.page().querySelector("input").disabled, true);
  finish(true);
  assert.equal(await saving, true);
  assert.equal(f.session.isDirty(f.page()), false);
  assert.match(f.status.textContent, /另有 1 页未保存/);
  f.select("system");
  assert.equal(f.page().querySelector("input").value, "en");
  assert.equal(f.session.isDirty(f.page()), true);
  f.dom.window.close();
});

test("refresh cannot adopt unsaved drafts; restoring the original value clears dirty state", () => {
  const f = fixture(async () => true);
  f.edit("2");
  f.dialog.removeAttribute("open");
  f.session.captureClean();
  f.dialog.setAttribute("open", "");
  assert.equal(f.page().querySelector("input").value, "2");
  assert.equal(f.session.isDirty(f.page()), true);
  assert.equal(f.doc.querySelector('[data-settings-tab="exploration"]').dataset.dirty, "true");
  f.edit("1");
  assert.equal(f.session.isDirty(f.page()), false);
  f.dom.window.close();
});

test("save failure keeps the draft and reports it on the owning page only", async () => {
  const f = fixture(async () => { throw new Error("服务暂不可用"); });
  f.edit("3");
  assert.equal(await f.session.save(), false);
  assert.equal(f.session.isDirty(f.page()), true);
  assert.match(f.status.textContent, /服务暂不可用/);
  assert.equal(f.doc.querySelector("#save").disabled, false);
  f.select("system");
  assert.doesNotMatch(f.status.textContent, /服务暂不可用/);
  f.select("exploration");
  assert.match(f.status.textContent, /服务暂不可用/);
  f.dom.window.close();
});

test("real settings keep a Luna draft across reopening and submit its exact values once", async (t) => {
  const html = await readFile(new URL("./index.html", import.meta.url), "utf8");
  const source = (await readFile(new URL("./settings.js", import.meta.url), "utf8"))
    .replace('"/presentation.js"', JSON.stringify(new URL("./presentation.js", import.meta.url).href))
    .replace('"/settings-session.js"', JSON.stringify(new URL("./settings-session.js", import.meta.url).href));
  const { initSettings } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
  const dom = new JSDOM(html);
  const prior = { document: globalThis.document, window: globalThis.window, fetch: globalThis.fetch };
  globalThis.document = dom.window.document;
  globalThis.window = dom.window;
  t.after(() => { Object.assign(globalThis, prior); dom.window.close(); });
  const dialog = document.querySelector("#settings-dialog");
  dialog.showModal = () => dialog.setAttribute("open", "");
  dialog.close = () => dialog.removeAttribute("open");
  const state = { models: [], ambient: { luna: { enabled: true, focus: "原来的观察范围", outputLanguage: "interface" }, providers: [], channels: [] } };
  const settings = initSettings(state);
  settings.open("sources");
  const focus = document.querySelector("#luna-focus");
  focus.value = "先提供具体来源事实";
  focus.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
  dialog.close();
  settings.open("sources");
  assert.equal(focus.value, "先提供具体来源事实");
  assert.match(document.querySelector("#settings-save-state").textContent, /未保存/);
  let finish;
  const requests = [];
  globalThis.fetch = (url, options) => {
    requests.push({ url, body: JSON.parse(options.body) });
    return new Promise((resolve) => { finish = resolve; });
  };
  document.querySelector("#settings-save").click();
  document.querySelector("#settings-save").click();
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, "/api/ambient");
  assert.equal(requests[0].body.luna.focus, "先提供具体来源事实");
  finish(new Response(JSON.stringify(requests[0].body)));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(document.querySelector("#settings-save").disabled, false);
  assert.match(document.querySelector("#settings-save-state").textContent, /已保存/);
});

test("live model catalog refresh updates clean settings without overwriting a model draft", async (t) => {
  const html = await readFile(new URL("./index.html", import.meta.url), "utf8");
  const source = (await readFile(new URL("./settings.js", import.meta.url), "utf8"))
    .replace('"/presentation.js"', JSON.stringify(new URL("./presentation.js", import.meta.url).href))
    .replace('"/settings-session.js"', JSON.stringify(new URL("./settings-session.js", import.meta.url).href));
  const { initSettings } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
  const dom = new JSDOM(html);
  const prior = { document: globalThis.document, window: globalThis.window };
  globalThis.document = dom.window.document;
  globalThis.window = dom.window;
  t.after(() => { Object.assign(globalThis, prior); dom.window.close(); });
  const model = (slug) => ({ id: slug, model: slug, displayName: slug,
    defaultReasoningEffort: "low", supportedReasoningEfforts: [{ reasoningEffort: "low" }] });
  const luna = model("gpt-5.6-luna"), sol = model("gpt-5.6-sol"), terra = model("gpt-5.6-terra");
  const state = { models: [luna, sol], compute: { routing: "bounded_auto",
    lanes: Object.fromEntries(["sense", "observe", "conversation", "investigate", "critical"].map((lane) => [lane, {model: luna.model, effort: "low"}])) } };
  const ui = initSettings(state);
  ui.renderCompute();
  const select = document.querySelector('[data-lane="sense"] [data-field="model"]');
  state.models = [luna, terra];
  ui.renderCompute();
  assert.deepEqual([...select.options].map((option) => option.value), [luna.model, terra.model]);
  select.value = terra.model;
  select.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  state.models = [luna, sol];
  ui.renderCompute();
  assert.equal(select.value, terra.model);
  assert.match(document.querySelector('[data-settings-tab="models"]').getAttribute("aria-label"), /未保存/);
});
