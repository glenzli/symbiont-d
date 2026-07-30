export function formatMemorySize(chars) {
  if (chars < 1000) return `${chars} chars`;
  return `${(chars / 1000).toFixed(1)}k chars`;
}

export function formatTokens(tokens) {
  if (tokens < 1000) return `${tokens} tok`;
  if (tokens < 1_000_000) {
    return `${(tokens / 1000).toFixed(tokens < 10000 ? 1 : 0)}k tok`;
  }
  return `${(tokens / 1_000_000).toFixed(tokens < 10_000_000 ? 1 : 0)}M tok`;
}

export function tokensToMillions(tokens) {
  const millions = Number(tokens || 0) / 1_000_000;
  return String(Number(millions.toFixed(6)));
}

export function millionsToTokens(millions) {
  const value = Number(millions);
  if (!Number.isFinite(value) || value <= 0) return 0;
  return Math.round(value * 1_000_000);
}

export function formatDuration(milliseconds) {
  if (milliseconds < 1000) return `${milliseconds}ms`;
  return `${(milliseconds / 1000).toFixed(milliseconds < 10000 ? 1 : 0)}s`;
}

export function formatDate(value) {
  if (!value) return "尚无";
  return new Date(value).toLocaleString();
}

export async function responseJson(response, fallback) {
  const payload = await response.json();
  if (!response.ok) throw new Error(payload.error || fallback);
  return payload;
}
