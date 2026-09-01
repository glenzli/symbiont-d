// Compatibility with persisted replies produced by the retired regex renderer.
// New math is parsed by MarkdownIt's TeX plugin, never replaced with placeholders.
const leakedMathPlaceholder =
  /<span\s+data-symbiont-math=(?:"\d+"|'\d+')\s*><\/span>/gi;

export function repairLeakedMathPlaceholders(markdown) {
  return String(markdown || "").replace(leakedMathPlaceholder, (token, offset, source) =>
    /^\s*\d/.test(source.slice(offset + token.length)) ? "$" : "",
  );
}
