import test from "node:test";
import assert from "node:assert/strict";

import {
  countConversationExternalInputs,
  conversationFocusPresentation,
  normalizeConversationFocus,
} from "./conversation-focus-ui.js";

test("normalizes only the persisted focused state", () => {
  assert.equal(normalizeConversationFocus("focused"), true);
  assert.equal(normalizeConversationFocus(true), true);
  assert.equal(normalizeConversationFocus("all"), false);
  assert.equal(normalizeConversationFocus(null), false);
});

test("focus presentation explains the inverse action and active count", () => {
  assert.deepEqual(conversationFocusPresentation(false, 8), {
    label: "只看对话与已采用的来源",
    tooltip: "聚焦对话",
    icon: "eye",
    visibleLabel: "聚焦",
  });
  assert.deepEqual(conversationFocusPresentation(true, 8), {
    label: "显示全部外部输入与异议",
    tooltip: "显示外部输入与异议",
    icon: "eye-off",
    visibleLabel: "聚焦中 · 隐藏 8 条",
  });
});

test("counts individual external inputs even when the renderer groups them", () => {
  const conversation = {
    querySelectorAll(selector) {
      assert.equal(selector, ".input-signal");
      return [{}, {}, {}];
    },
  };
  assert.equal(countConversationExternalInputs(conversation), 3);
});
