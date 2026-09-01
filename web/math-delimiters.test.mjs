import assert from "node:assert/strict";
import test from "node:test";
import { repairLeakedMathPlaceholders } from "./math-delimiters.mjs";

test("repairs old currency placeholders without inventing missing expressions", () => {
  assert.equal(repairLeakedMathPlaceholders(
    'Prices: <span data-symbiont-math="0"></span>0.14 / <span data-symbiont-math="1"></span>0.007; <span data-symbiont-math="2"></span>done.',
  ), "Prices: $0.14 / $0.007; done.");
});
