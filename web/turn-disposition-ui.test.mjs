import assert from "node:assert/strict";
import test from "node:test";

import { dispositionState } from "./turn-disposition-ui.js";

test("turn disposition maps reactions and silent settlement to distinct UI states", () => {
  assert.equal(dispositionState({ reaction: "👍" }), "reacted");
  assert.equal(dispositionState({ reaction: "  " }), "settled");
  assert.equal(dispositionState({}), "settled");
});
