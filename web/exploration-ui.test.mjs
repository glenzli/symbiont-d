import assert from "node:assert/strict";
import test from "node:test";

import {
  manualCompletionNotice,
  manualCompletionSince,
  manualRunLabel,
  manualRunPending,
  unpresentedManualCompletions,
} from "./exploration-receipt.js";

test("describes external inputs without presenting them as Symbiont output", () => {
  assert.deepEqual(
    manualCompletionNotice({ outcome: "input_signals_broadcast" }),
    {
      label: "探索完成",
      message: "已带回新的广域输入，可以直接回复；它们仍保持外部输入身份。",
    },
  );
});

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
    {
      manualRun: {
        id: "explore_1",
        status: "silent",
        completedAt: "2026-08-04T15:00:00Z",
        presentedAt: "2026-08-04T15:00:01Z",
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

test("restores every durable completion that has not been presented", () => {
  const completions = unpresentedManualCompletions({
    manualReceipts: [
      {
        id: "explore_silent",
        status: "silent",
        completedAt: "2026-08-06T00:00:01Z",
        outcome: "silent",
        presentedAt: null,
      },
      {
        id: "explore_seen",
        status: "failed",
        completedAt: "2026-08-06T00:00:02Z",
        outcome: "failed",
        presentedAt: "2026-08-06T00:00:03Z",
      },
      {
        id: "explore_failed",
        status: "failed",
        completedAt: "2026-08-06T00:00:04Z",
        outcome: "failed",
        reason: "service_restarted",
      },
    ],
  });

  assert.deepEqual(
    completions.map((completion) => completion.id),
    ["explore_silent", "explore_failed"],
  );
  assert.equal(completions[1].reason, "service_restarted");
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
