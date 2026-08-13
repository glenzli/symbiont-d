import assert from "node:assert/strict";
import test from "node:test";

import { briefingEntries, briefingRoleProjection } from "./input-briefing-ui.js";

const roles = [
  { id: "luna", name: "Luna" },
  { id: "mail", name: "Research Inbox" },
];

const input = (id, roleId) => ({
  id,
  kind: "external_input",
  actor: { id: roleId },
});

test("briefing lists only roles that actually delivered external input", () => {
  assert.deepEqual(
    briefingRoleProjection([input("a", "luna")], roles).map(({ id, count }) => ({ id, count })),
    [{ id: "luna", count: 1 }],
  );
});

test("briefing keeps a dissent beside only its related input role", () => {
  const signals = [
    input("a", "luna"),
    input("b", "mail"),
    { id: "d", kind: "attacker_challenge", actor: { id: "attacker" }, relatedSignalIds: ["a"] },
  ];
  assert.deepEqual(
    briefingEntries(signals, "luna").map((signal) => signal.id),
    ["a", "d"],
  );
  assert.deepEqual(
    briefingEntries(signals, "mail").map((signal) => signal.id),
    ["b"],
  );
});
