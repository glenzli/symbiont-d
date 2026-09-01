import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import createPurifier from "dompurify";
import { renderMarkdownInto } from "./markdown-renderer.mjs";

function render(source) {
  const { window } = new JSDOM("<main></main>");
  const element = window.document.querySelector("main");
  renderMarkdownInto(element, source, createPurifier(window));
  return element;
}

test("single-backslash formulas survive bold headings, lists and tables", () => {
  const root = render(String.raw`**主系列 $\mathrm{GL}_n \times \mathrm{GL}_n$**

- 双重局部域 $\mathbb{F}_q((t_1))((t_2))$，$\mathrm{Hom}$-值形式

| notation | meaning |
| --- | --- |
| $\mathfrak{S}C_\alpha(v)$ | 测试 |

\(f^*, f_*, f_!, f^!, \otimes^\mathbb{L}, \mathcal{R}\mathcal{H}om\)

\[
\Delta J_t = \langle g_t,\delta_c\rangle
\]

$$x^2 + y^2$$
`);
  assert.equal(root.querySelectorAll(".katex").length, 7);
  assert.equal(root.querySelectorAll(".katex-error").length, 0);
  assert.equal(root.querySelectorAll("math").length, 7);
  assert.ok(root.querySelector("table .katex"));
  assert.ok(root.querySelector("strong .katex"));
});

test("code, escaped dollars and prices are not math", () => {
  const root = render(String.raw`Price $0.14 / $0.007 / $0.66, \$5, and $J_t$.

Inline code: ` + "`$\\mathrm{GL}_n$`\n\n```js\nconst x = '$a_b$';\n```\n");
  assert.equal(root.querySelectorAll(".katex").length, 1);
  assert.match(root.textContent, /\$0\.14 \/ \$0\.007 \/ \$0\.66/);
  assert.match(root.querySelector("code").textContent, /\$\\mathrm/);
  assert.match(root.querySelector("pre").textContent, /\$a_b\$/);
});

test("math fences, numeric formulas, alignment and incomplete streaming text", () => {
  const root = render("```math\n\\begin{aligned}x&=1\\\\ y&=2\\end{aligned}\n```\n\n$1 + 1$, $2^n$, then $\\frac{");
  assert.equal(root.querySelectorAll(".katex").length, 3);
  assert.equal(root.querySelectorAll(".katex-error").length, 0);
  assert.match(root.textContent, /\$\\frac\{/);
  assert.doesNotMatch(root.innerHTML, /data-symbiont-math/);
});

test("malformed math remains visible and untrusted HTML or TeX cannot execute", () => {
  const root = render(String.raw`<img src=x onerror="alert(1)"><script>alert(1)</script>

$\frac{1}$ $\href{javascript:alert(1)}{click}$

[link](https://example.test) [evil](javascript:alert(1))`);
  assert.ok(root.querySelector(".katex-error"));
  assert.equal(root.querySelector("script, [onerror], a[href^='javascript:']"), null);
  assert.equal(root.querySelector("a").rel, "noopener noreferrer");
});

test("legacy double-escaped hash is repaired only after standard TeX rejects it", () => {
  const root = render(String.raw`$M_k(N) := \\#\{x_1\cdots x_k : x_i \in \{1, \ldots, N\}\}$

$$\begin{aligned} x &= 1 \\ & = 2 \end{aligned}$$`);
  assert.equal(root.querySelectorAll(".katex").length, 2);
  assert.equal(root.querySelectorAll(".katex-error").length, 0);
  assert.match(root.querySelectorAll("annotation")[1].textContent, /\\\\ &/);
});
