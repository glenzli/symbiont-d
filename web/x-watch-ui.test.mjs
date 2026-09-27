import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { JSDOM } from "jsdom";
import { initXWatchUi } from "./x-watch-ui.js";

test("X watch saves a rule and starts a user-directed browser check", async (t) => {
  const html = await readFile(new URL("./index.html", import.meta.url), "utf8");
  const dom = new JSDOM(html);
  const prior = {
    document: globalThis.document, window: globalThis.window,
    CustomEvent: globalThis.CustomEvent, fetch: globalThis.fetch,
  };
  Object.assign(globalThis, {
    document: dom.window.document, window: dom.window,
    CustomEvent: dom.window.CustomEvent,
  });
  t.after(() => { Object.assign(globalThis, prior); dom.window.close(); });
  const requests = [];
  const state = { xWatches: { watches: [] } };
  globalThis.fetch = async (url, options) => {
    requests.push({ url, payload: JSON.parse(options.body) });
    state.xWatches = { watches: [{
      id: "watch_1", handle: "Example", focus: "模型发布", delivery: "digest",
      enabled: true, status: "waiting_first_check", recentPosts: [{
        id: "12345678", url: "https://x.com/Example/status/12345678",
        text: "<img src=x onerror=alert(1)> 新帖", postedAt: null,
      }],
    }] };
    return Response.json(state.xWatches);
  };
  const ui = initXWatchUi(state);
  ui.render();
  assert.equal(document.querySelector("#x-watch-check-all").disabled, true);
  document.querySelector("#x-watch-new-handle").value = "@Example";
  document.querySelector("#x-watch-new-focus").value = "模型发布";
  document.querySelector("#x-watch-add").click();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(requests[0], { url: "/api/x-watches", payload: {
    action: "add", handle: "@Example", focus: "模型发布", delivery: "digest",
  } });
  assert.equal(document.querySelector(".x-watch-row strong").textContent, "@Example");
  assert.equal(document.querySelector(".x-watch-recent img"), null);
  assert.match(document.querySelector(".x-watch-recent a").textContent, /<img/);
  let checked;
  document.addEventListener("symbiont:x-watch-check", (event) => { checked = event.detail.key; });
  document.querySelector(".x-watch-controls button:nth-child(2)").click();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(checked, "watch_1");
  document.querySelector("#x-watch-check-all").click();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(checked, null);
  assert.doesNotMatch(document.querySelector("#x-watch-form").textContent, /Bearer Token/);
});
