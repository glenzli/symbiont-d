export function formatMemorySize(chars) {
  if (chars < 1000) return `${chars} chars`;
  return `${(chars / 1000).toFixed(1)}k chars`;
}

export function formatTokens(tokens) {
  if (tokens < 1000) return `${tokens} tok`;
  return `${(tokens / 1000).toFixed(tokens < 10000 ? 1 : 0)}k tok`;
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
