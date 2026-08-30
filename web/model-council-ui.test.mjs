import assert from "node:assert/strict";
import test from "node:test";

import { mentionedModelIds, modelMentionAt } from "./model-council-ui.js";

const participants = [
  { id: "claude", name: "Claude", enabled: true },
  { id: "deep-seek", name: "DeepSeek", enabled: true },
  { id: "disabled", name: "Disabled", enabled: false },
];

test("mention completion recognizes a stable participant id at the caret", () => {
  assert.deepEqual(modelMentionAt("请 @cla", 6), {
    start: 2,
    end: 6,
    query: "cla",
  });
  assert.equal(modelMentionAt("mail@example", 12), null);
});

test("only enabled participant mentions outside code activate models", () => {
  assert.deepEqual(
    mentionedModelIds(
      "@claude 看一下，@deep-seek 也参与。`@disabled`\n```\n@claude\n```",
      participants,
    ),
    ["claude", "deep-seek"],
  );
});

test("escaped mentions and email-like text do not activate models", () => {
  assert.deepEqual(
    mentionedModelIds("\\@claude 与 owner@deep-seek 都只是文本", participants),
    [],
  );
});
