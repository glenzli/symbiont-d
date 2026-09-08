import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";

import { availableMessageActions, initMessageActions } from "./message-actions.js";

test("stopped user messages remain editable", () => {
  assert.deepEqual(
    availableMessageActions({
      role: "user",
      state: "stopped",
      hasContent: true,
    }),
    ["copy", "edit", "retry", "delete"],
  );
});

test("completed user messages retain their edit and recall actions", () => {
  assert.deepEqual(
    availableMessageActions({
      role: "user",
      state: "delivered",
      hasRevision: true,
      hasContent: true,
    }),
    ["quote", "copy", "edit", "recall"],
  );
});

test("busy and action-busy states suppress message mutations", () => {
  assert.deepEqual(
    availableMessageActions({
      role: "user",
      state: "stopped",
      hasContent: true,
      busy: true,
    }),
    ["copy"],
  );
  assert.deepEqual(
    availableMessageActions({
      role: "user",
      state: "stopped",
      hasContent: true,
      actionBusy: true,
    }),
    [],
  );
});

test("clipboard failure does not turn an answered message into a retryable turn", async (t) => {
  const dom = new JSDOM(`<section id="conversation"><article class="message" data-role="user">
    <footer class="message-foot"><span class="message-state"></span><div class="message-actions"></div></footer>
  </article></section>`);
  const previousDocument = globalThis.document;
  globalThis.document = dom.window.document;
  t.after(() => { globalThis.document = previousDocument; dom.window.close(); });
  const conversation = document.querySelector("#conversation");
  const message = conversation.querySelector("article");
  const actions = initMessageActions({ conversation, isBusy: () => false,
    perform: async () => { throw new Error("剪贴板不可用"); } });
  actions.track(message, { role: "user", revisionId: "original", content: "已经回答的问题" });
  message.querySelector('[data-message-action="copy"]').click();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(message.dataset.deliveryState, "delivered");
  assert.equal(message.classList.contains("message-failed"), false);
  assert.equal(message.querySelector('[data-message-action="retry"]'), null);
  assert.ok(message.querySelector('[data-message-action="recall"]'));
  assert.match(message.querySelector(".message-state").textContent, /操作失败：剪贴板不可用/);
});
