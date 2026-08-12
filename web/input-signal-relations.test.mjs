import assert from "node:assert/strict";
import test from "node:test";

import { localRelationGeometry } from "./input-signal-relations.js";

const viewport = { top: 100, bottom: 700, left: 20 };

test("connects two visible nearby avatars", () => {
  const relation = localRelationGeometry(
    { top: 180, bottom: 220, left: 40, width: 40 },
    { top: 320, bottom: 360, left: 40, width: 40 },
    viewport,
  );
  assert.ok(relation);
  assert.match(relation.path, /^M 40 123 C /);
});

test("uses the source card bottom so long content does not create a false long span", () => {
  const relation = localRelationGeometry(
    { top: 180, bottom: 420, left: 40, width: 40 },
    { top: 580, bottom: 620, left: 40, width: 40 },
    viewport,
  );
  assert.ok(relation);
  assert.match(relation.path, /^M 40 323 C /);
});

test("does not draw a connector across a distant conversation span", () => {
  assert.equal(
    localRelationGeometry(
      { top: 120, bottom: 160, left: 40, width: 40 },
      { top: 620, bottom: 660, left: 40, width: 40 },
      viewport,
    ),
    null,
  );
});

test("does not connect hidden or reverse-ordered messages", () => {
  assert.equal(
    localRelationGeometry(
      { top: 20, bottom: 60, left: 40, width: 40 },
      { top: 180, bottom: 220, left: 40, width: 40 },
      viewport,
    ),
    null,
  );
  assert.equal(
    localRelationGeometry(
      { top: 300, bottom: 340, left: 40, width: 40 },
      { top: 220, bottom: 260, left: 40, width: 40 },
      viewport,
    ),
    null,
  );
});
