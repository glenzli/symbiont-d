import assert from "node:assert/strict";
import test from "node:test";

import { availableMessageActions } from "./message-actions.js";

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
