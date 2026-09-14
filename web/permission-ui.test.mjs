import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { initPermissionUi } from "./permission-ui.js";

function fixture(t, request) {
  const dom = new JSDOM('<section id="permission-center"><div id="permission-list"></div><span id="permission-status"></span></section>');
  const previous = globalThis.document;
  globalThis.document = dom.window.document;
  t.after(() => { globalThis.document = previous; dom.window.close(); });
  const state = { permissions: [{ id: "request", title: "读取网页", source: "symbiont", kind: "networkAccess",
    expiresAt: new Date(Date.now() + 60000).toISOString(), allowAccept: true, allowSession: true, details: {}, ...request }] };
  const ui = initPermissionUi(state);
  ui.render();
  return { state, ui, doc: dom.window.document };
}

test("permission links only navigate to web URLs, and session scope names its real lifetime", (t) => {
  const f = fixture(t, { kind: "mcpElicitation", host: "javascript:alert(1)" });
  assert.equal(f.doc.querySelector("a"), null);
  assert.match(f.doc.body.textContent, /javascript:alert/);
  assert.match(f.doc.body.textContent, /允许至服务重启/);
  assert.match(f.doc.body.textContent, /包括后台读取/);
  f.state.permissions[0].host = "https://example.test/authorize";
  f.ui.render();
  assert.equal(f.doc.querySelector("a").href, "https://example.test/authorize");
});

test("a runtime refresh cannot re-enable an in-flight permission decision", async (t) => {
  const f = fixture(t);
  const previousFetch = globalThis.fetch;
  let finish;
  let calls = 0;
  globalThis.fetch = () => { calls++; return new Promise((resolve) => { finish = resolve; }); };
  t.after(() => { globalThis.fetch = previousFetch; });
  f.doc.querySelector("button").click();
  f.ui.render();
  assert.ok([...f.doc.querySelectorAll("button")].every((button) => button.disabled));
  f.doc.querySelector("button").click();
  assert.equal(calls, 1);
  finish(new Response("{}"));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(f.state.permissions.length, 0);
  assert.equal(f.doc.querySelector("#permission-center").hidden, true);
});

test("browser confirmation offers explicit persistent acceptance and sends the selected decision", async (t) => {
  const f = fixture(t, { source: "codex", kind: "mcpElicitation", allowSession: false, allowCancel: true,
    host: "https://huggingface.co", details: { mode: "form", requestedSchema: { type: "object", properties: {} }, _meta: { persist: "always" } } });
  assert.deepEqual([...f.doc.querySelectorAll("button")].map(b => b.textContent), ["持续允许", "拒绝", "停止操作"]);
  assert.match(f.doc.body.textContent, /会保存授权/);
  assert.equal(f.doc.querySelector("a").href, "https://huggingface.co/");
  const previousFetch = globalThis.fetch;
  let submitted;
  globalThis.fetch = async (url, options) => { submitted = { url, body: JSON.parse(options.body) }; return Response.json({}); };
  t.after(() => globalThis.fetch = previousFetch);
  assert.equal(submitted, undefined, "rendering must never grant approval");
  f.doc.querySelector("button").click();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(submitted, { url: "/api/permissions/request", body: { decision: "accept" } });
  assert.equal(f.state.permissions.length, 0);
});

test("unsupported forms explain the missing accept action", (t) => {
  const f = fixture(t, { source: "codex", kind: "mcpElicitation", allowAccept: false, allowSession: false, allowCancel: true });
  assert.deepEqual([...f.doc.querySelectorAll("button")].map(b => b.textContent), ["拒绝", "停止操作"]);
  assert.match(f.doc.body.textContent, /不支持的表单/);
});
