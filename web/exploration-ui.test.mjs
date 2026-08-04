import assert from "node:assert/strict";
import test from "node:test";

import { manualCompletionSince } from "./exploration-receipt.js";

test("returns a receipt only for a newer completed manual exploration", () => {
  const completion = manualCompletionSince(
    {
      phase: "waiting",
      lastRunAt: "2026-08-04T15:00:00Z",
      lastTrigger: "manual",
      lastOutcome: "silent",
    },
    "2026-08-04T14:00:00Z",
  );

  assert.deepEqual(completion, {
    id: "2026-08-04T15:00:00Z",
    completedAt: "2026-08-04T15:00:00Z",
    outcome: "silent",
  });
});

test("does not turn scheduled, in-progress, or previous runs into a manual receipt", () => {
  const priorRunAt = "2026-08-04T14:00:00Z";
  for (const exploration of [
    {
      phase: "waiting",
      lastRunAt: "2026-08-04T15:00:00Z",
      lastTrigger: "scheduled",
      lastOutcome: "silent",
    },
    {
      phase: "exploring",
      lastRunAt: "2026-08-04T15:00:00Z",
      lastTrigger: "manual",
      lastOutcome: "silent",
    },
    {
      phase: "waiting",
      lastRunAt: priorRunAt,
      lastTrigger: "manual",
      lastOutcome: "silent",
    },
  ]) {
    assert.equal(manualCompletionSince(exploration, priorRunAt), null);
  }
});
