import assert from "node:assert/strict";
import test from "node:test";

import {
  briefingDateKey,
  briefingEntries,
  briefingRoleProjection,
  briefingTopicStatus,
  briefingTopicEntries,
  briefingTopicProjection,
  briefingTopicRunNotice,
} from "./input-briefing-ui.js";

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

test("briefing roles and entries are scoped to the selected local date", () => {
  const signals = [
    { ...input("today", "luna"), observedAt: "2026-08-14T01:00:00Z" },
    { ...input("older", "mail"), observedAt: "2026-08-13T01:00:00Z" },
    { id: "dissent", kind: "attacker_challenge", relatedSignalIds: ["today"] },
  ];
  const selectedDate = briefingDateKey(signals[0]);
  assert.deepEqual(
    briefingRoleProjection(signals, roles, selectedDate).map(({ id, count }) => ({ id, count })),
    [{ id: "luna", count: 1 }],
  );
  assert.deepEqual(
    briefingEntries(signals, "luna", selectedDate).map((signal) => signal.id),
    ["today", "dissent"],
  );
});

test("briefing orders a role's selected-day inputs by observed time", () => {
  const signals = [
    { ...input("later", "luna"), observedAt: "2026-08-14T03:00:00Z" },
    { ...input("earlier", "luna"), observedAt: "2026-08-14T01:00:00Z" },
  ];
  const selectedDate = briefingDateKey(signals[0]);
  assert.deepEqual(
    briefingEntries(signals, "luna", selectedDate).map((signal) => signal.id),
    ["earlier", "later"],
  );
});

test("topic axis groups local-day input and keeps related dissent with its source", () => {
  const signals = [
    { ...input("ai", "luna"), briefingTopic: "本地模型", observedAt: "2026-08-14T01:00:00Z" },
    { ...input("paper", "mail"), observedAt: "2026-08-14T02:00:00Z" },
    { id: "dissent", kind: "attacker_challenge", relatedSignalIds: ["ai"] },
  ];
  const selectedDate = briefingDateKey(signals[0]);
  assert.deepEqual(
    briefingTopicProjection(signals, selectedDate).map(({ id, count }) => ({ id, count })),
    [{ id: "本地模型", count: 1 }, { id: "未归类", count: 1 }],
  );
  assert.deepEqual(
    briefingTopicEntries(signals, "本地模型", selectedDate).map((signal) => signal.id),
    ["ai", "dissent"],
  );
});

test("topic status distinguishes local work waiting from an unavailable local runtime", () => {
  assert.equal(briefingTopicStatus({ briefingTopicStatus: "pending" }), "pending");
  assert.equal(briefingTopicStatus({ briefing_topic_status: "unavailable" }), "unavailable");
  assert.equal(briefingTopicStatus({ briefingTopic: "本地模型" }), "classified");
  assert.equal(briefingTopicStatus({}), "unclassified");
});

test("topic organization reports completion and retryable local failures distinctly", () => {
  assert.deepEqual(
    briefingTopicRunNotice({ outcome: "completed", queuedCount: 4, assignedCount: 3 }),
    { problem: false, text: "已整理 4 条：3 条归入主题，其余保留为未归类。" },
  );
  assert.deepEqual(
    briefingTopicRunNotice({ outcome: "completed", queuedCount: 4, assignedCount: 3, reclassified: true }),
    { problem: false, text: "已重新整理 4 条：3 条归入主题，其余保留为未归类。" },
  );
  assert.match(briefingTopicRunNotice({ outcome: "deferred" }).text, /仍可重试/);
  assert.equal(briefingTopicRunNotice({ outcome: "deferred" }).problem, true);
});
