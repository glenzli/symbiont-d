import assert from "node:assert/strict";
import test from "node:test";

import { inputSignalGroupRuns } from "./input-signal-groups.js";

const signal = (roleId, minute) => ({
  isSignal: true,
  roleId,
  observedAt: `2026-08-10T01:${String(minute).padStart(2, "0")}:00Z`,
});

test("groups two consecutive nearby inputs from the same role", () => {
  assert.deepEqual(
    inputSignalGroupRuns([signal("mail", 0), signal("mail", 5)]),
    [{ start: 0, end: 2 }],
  );
});

test("does not cross a normal conversation message", () => {
  assert.deepEqual(
    inputSignalGroupRuns([
      signal("mail", 0),
      { isSignal: false },
      signal("mail", 3),
    ]),
    [],
  );
});

test("does not combine different roles or distant inputs", () => {
  assert.deepEqual(
    inputSignalGroupRuns([
      signal("mail", 0),
      signal("luna", 1),
      signal("luna", 30),
    ]),
    [],
  );
});

test("retains every input in a continuous group", () => {
  assert.deepEqual(
    inputSignalGroupRuns([signal("mail", 0), signal("mail", 5), signal("mail", 9)]),
    [{ start: 0, end: 3 }],
  );
});
