import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { build } from "esbuild";
import { JSDOM } from "jsdom";

const web = path.dirname(fileURLToPath(import.meta.url));
const html = await readFile(path.join(web, "index.html"), "utf8");
const servedModules = {
  name: "served-web-modules",
  setup(builder) {
    builder.onResolve({ filter: /^\// }, ({ path: url, kind }) => {
      if (kind !== "entry-point") return { path: path.join(web, url) };
    });
  },
};

test("retired controls are absent while conversation reflection remains available", () => {
  const dom = new JSDOM(html);
  const doc = dom.window.document;
  assert.equal(doc.querySelector('[id*="reconciliation"], [id^="memory-index"]'), null);
  for (const selector of ["#reflection-form", "#run-reflection", "#reflection-archive", "#open-explorations"]) {
    assert.ok(doc.querySelector(selector), selector);
  }
  for (const control of doc.querySelectorAll("[data-close]")) {
    assert.ok(doc.getElementById(control.dataset.close), control.dataset.close);
  }
  dom.window.close();
});

test("application module graph no longer loads the retired UI", async () => {
  const result = await build({
    entryPoints: [path.join(web, "app.js")], bundle: true, write: false,
    format: "esm", metafile: true, plugins: [servedModules], logLevel: "silent",
  });
  const inputs = Object.keys(result.metafile.inputs);
  assert.ok(inputs.some(name => name.endsWith("reflection-ui.js")));
  assert.ok(inputs.some(name => name.endsWith("context-inspector.js")));
  assert.ok(!inputs.some(name => name.includes("reconciliation")));
});

test("remaining immediate reflection action still calls its own endpoint", async () => {
  const result = await build({
    entryPoints: [path.join(web, "reflection-ui.js")], bundle: true, write: false,
    format: "esm", plugins: [servedModules], logLevel: "silent",
  });
  const dom = new JSDOM(html, { url: "http://symbiont.test/" });
  const previous = { window: globalThis.window, document: globalThis.document, fetch: globalThis.fetch };
  const calls = [];
  try {
    globalThis.window = dom.window;
    globalThis.document = dom.window.document;
    globalThis.fetch = async (url, options) => {
      calls.push([url, options?.method || "GET"]);
      assert.equal(url, "/api/reflection/run");
      return { ok: true, json: async () => ({ accepted: true }) };
    };
    const module = await import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString("base64")}`);
    const ui = module.initReflectionUi({ reflection: { runtime: { phase: "waiting" } } });
    ui.render();
    dom.window.document.querySelector("#run-reflection").click();
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(calls, [["/api/reflection/run", "POST"]]);
  } finally {
    Object.assign(globalThis, previous);
    dom.window.close();
  }
});
