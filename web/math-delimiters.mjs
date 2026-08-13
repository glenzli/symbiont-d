const delimiters = [
  {
    // Keep simple numeric formulas available too (`$1 + 1$`, `$2^n$`), while
    // deliberately excluding a bare price followed by a slash (`$0.14 / $`).
    pattern: /(?<!\\)\$(?=\d+(?:\.\d+)?\s*[+\-*=^_({\[])([^$\n]+?)(?<!\\)\$/g,
    display: false,
  },
  {
    pattern: /(?<!\\)\\\[([\s\S]+?)\\\]/g,
    display: true,
  },
  {
    pattern: /(?<!\\)\$\$([\s\S]+?)(?<!\\)\$\$/g,
    display: true,
  },
  {
    pattern: /(?<!\\)\\\(([\s\S]+?)\\\)/g,
    display: false,
  },
  {
    // A currency amount such as `$0.14 / $0.007 / $0.66` is not inline math.
    // Requiring an identifier or a TeX command after the opening dollar keeps
    // ordinary prices intact while retaining `$J_t$` and `$\\frac{1}{2}$`.
    pattern: /(?<!\\)\$(?=\s*(?:[A-Za-z(]|\\\\))([^$\n]+?)(?<!\\)\$/g,
    display: false,
  },
];

const leakedMathPlaceholder =
  /<span\s+data-symbiont-math=(?:"\d+"|'\d+')\s*><\/span>/gi;

export function protectMath(markdown) {
  const math = [];
  let protectedMarkdown = String(markdown || "");

  for (const delimiter of delimiters) {
    protectedMarkdown = protectedMarkdown.replace(
      delimiter.pattern,
      (_, expression) => mathToken(math, expression, delimiter.display),
    );
  }

  return { protectedMarkdown, math };
}

function mathToken(math, expression, display) {
  const index = math.push({ expression: expression.trim(), display }) - 1;
  return `<span data-symbiont-math="${index}"></span>`;
}

// Older streamed replies can contain the renderer's private placeholder in the
// persisted message text. A placeholder immediately before a number came from
// a currency dollar that was misread as math; restore that dollar. Other stale
// placeholders carry no expression, so drop them rather than exposing markup.
export function repairLeakedMathPlaceholders(markdown) {
  return String(markdown || "").replace(leakedMathPlaceholder, (token, offset, source) =>
    /^\s*\d/.test(source.slice(offset + token.length)) ? "$" : "",
  );
}
