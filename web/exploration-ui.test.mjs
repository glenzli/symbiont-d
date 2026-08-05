import assert from "node:assert/strict";
import test from "node:test";

import {
  manualCompletionSince,
  manualRunLabel,
  manualRunPending,
} from "./exploration-receipt.js";

test("returns a receipt only for a newer completed manual exploration", () => {
  const completion = manualCompletionSince(
    {
      phase: "waiting",
      manualRun: {
        id: "explore_1",
        status: "silent",
        completedAt: "2026-08-04T15:00:00Z",
        outcome: "silent",
      },
    },
    "explore_1",
  );

  assert.deepEqual(completion, {
    id: "explore_1",
    completedAt: "2026-08-04T15:00:00Z",
    outcome: "silent",
    reason: null,
    resultRevisionId: null,
  });
});

test("does not turn queued, in-progress, or another manual run into a receipt", () => {
  for (const exploration of [
    {
      manualRun: {
        id: "explore_1",
        status: "queued",
        completedAt: null,
      },
    },
    {
      manualRun: {
        id: "explore_1",
        status: "exploring",
        completedAt: null,
      },
    },
    {
      manualRun: {
        id: "explore_2",
        status: "silent",
        completedAt: "2026-08-04T15:00:00Z",
      },
    },
  ]) {
    assert.equal(manualCompletionSince(exploration, "explore_1"), null);
  }
});

test("returns failed and messaged manual completions with their stable identity", () => {
  for (const [status, outcome] of [
    ["failed", "failed"],
    ["messaged", "messaged_discussion"],
  ]) {
    const completion = manualCompletionSince(
      {
        manualRun: {
          id: "explore_3",
          status,
          completedAt: "2026-08-04T16:00:00Z",
          outcome,
          reason: status === "failed" ? "runtime_error" : null,
          resultRevisionId: status === "messaged" ? "rev_result" : null,
        },
      },
      "explore_3",
    );
    assert.equal(completion.outcome, outcome);
    assert.equal(completion.reason, status === "failed" ? "runtime_error" : null);
  }
});

test("projects queued manual work consistently across exploration surfaces", () => {
  const exploration = {
    manualRun: { status: "queued", reason: "newer_user_input" },
  };
  assert.equal(manualRunPending(exploration), true);
  assert.equal(
    manualRunLabel(exploration.manualRun),
    "已优先处理新消息，稍后继续探索",
  );
  assert.equal(manualRunPending({ manualRun: { status: "silent" } }), false);
});
