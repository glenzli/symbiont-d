import assert from "node:assert/strict";
import test from "node:test";

import { temporaryDiscussionState } from "./ephemeral-discussion-ui.js";

test("a failed unanswered turn is retryable", () => {
  assert.deepEqual(
    temporaryDiscussionState({
      active: true,
      held: false,
      busy: false,
      turns: [
        {
          role: "user",
          content: "保留我",
          failure: "runtime unavailable",
        },
      ],
    }),
    { active: true, held: false, busy: false, retryable: true },
  );
});

test("an answered or active turn is not retryable", () => {
  assert.equal(
    temporaryDiscussionState({
      active: true,
      turns: [
        { role: "user", content: "问题", failure: "old failure" },
        { role: "assistant", content: "回答" },
      ],
    }).retryable,
    false,
  );
  assert.equal(
    temporaryDiscussionState({ active: true, busy: true, turns: [] }).retryable,
    false,
  );
});
