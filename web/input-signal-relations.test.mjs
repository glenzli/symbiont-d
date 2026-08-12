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
  assert.equal(relation.gutterX, 16);
  assert.equal(
    relation.path,
    "M 19 100 H 17.5 Q 16 100 16 101.5 V 238.5 Q 16 240 17.5 240 H 19",
  );
});

test("anchors the route to both avatar centers across a tall source card", () => {
  const relation = localRelationGeometry(
    { top: 180, bottom: 220, left: 40, width: 40 },
    { top: 580, bottom: 620, left: 40, width: 40 },
    viewport,
  );
  assert.ok(relation);
  assert.match(relation.path, /^M 19 100 H 17\.5 Q 16 100 16 101\.5 V /);
  assert.equal(relation.startY, 100);
  assert.equal(relation.endX, 19);
  assert.equal(relation.endY, 500);
});

test("does not draw a connector across a distant conversation span", () => {
  assert.equal(
    localRelationGeometry(
      { top: 120, bottom: 160, left: 40, width: 40 },
      { top: 1720, bottom: 1760, left: 40, width: 40 },
      { ...viewport, bottom: 1900 },
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
