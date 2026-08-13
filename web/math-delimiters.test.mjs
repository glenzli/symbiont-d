import assert from "node:assert/strict";
import test from "node:test";

import { protectMath, repairLeakedMathPlaceholders } from "./math-delimiters.mjs";

test("protects dollar and LaTeX math delimiters", () => {
  const source = String.raw`
\[
O_c = O+\delta_c
\]

Inline: \(g_t\), $J_t$, and:

$$
\Delta J_t \approx \langle g_t,\delta_c\rangle
$$
`;

  const result = protectMath(source);

  assert.deepEqual(result.math, [
    { expression: String.raw`O_c = O+\delta_c`, display: true },
    {
      expression: String.raw`\Delta J_t \approx \langle g_t,\delta_c\rangle`,
      display: true,
    },
    { expression: "g_t", display: false },
    { expression: "J_t", display: false },
  ]);
  assert.match(result.protectedMarkdown, /data-symbiont-math="0"/);
  assert.match(result.protectedMarkdown, /data-symbiont-math="3"/);
});

test("leaves escaped delimiters and ordinary brackets unchanged", () => {
  const source = String.raw`Keep \\[literal\\], \[math\], [ordinary], and \$5.`;
  const result = protectMath(source);

  assert.deepEqual(result.math, [{ expression: "math", display: true }]);
  assert.match(result.protectedMarkdown, /\\\\\[literal\\\\\]/);
  assert.match(result.protectedMarkdown, /\[ordinary\]/);
  assert.match(result.protectedMarkdown, /\\\$5/);
});

test("does not treat multiline single-dollar content as inline math", () => {
  const source = "$first line\nsecond line$";
  const result = protectMath(source);

  assert.deepEqual(result.math, []);
  assert.equal(result.protectedMarkdown, source);
});

test("keeps currency amounts out of inline math while preserving nearby formulas", () => {
  const source =
    "V4 Flash is $0.14 / $0.007 / $0.66, while $J_t$ and $1 + 1$ remain formulas.";
  const result = protectMath(source);

  assert.deepEqual(result.math, [
    { expression: "1 + 1", display: false },
    { expression: "J_t", display: false },
  ]);
  assert.match(result.protectedMarkdown, /\$0\.14 \/ \$0\.007 \/ \$0\.66/);
  assert.match(result.protectedMarkdown, /data-symbiont-math="1"/);
});

test("repairs leaked currency placeholders and removes expressionless markers", () => {
  const source =
    'Prices: <span data-symbiont-math="0"></span>0.14 / <span data-symbiont-math="1"></span>0.007; <span data-symbiont-math="2"></span>done.';

  assert.equal(repairLeakedMathPlaceholders(source), "Prices: $0.14 / $0.007; done.");
});
