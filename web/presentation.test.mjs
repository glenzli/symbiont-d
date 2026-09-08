import assert from "node:assert/strict";
import test from "node:test";

import {
  formatTokens,
  millionsToTokens,
  tokensToMillions,
  responseJson,
} from "./presentation.js";

test("token counts use familiar Chinese quantity units", () => {
  assert.equal(formatTokens(999), "999 tok");
  assert.equal(formatTokens(9_999), "9,999 tok");
  assert.equal(formatTokens(10_000), "1万 tok");
  assert.equal(formatTokens(12_345), "1.2万 tok");
  assert.equal(formatTokens(123_456), "12.3万 tok");
  assert.equal(formatTokens(1_234_567), "123.5万 tok");
  assert.equal(formatTokens(12_345_678), "1234.6万 tok");
  assert.equal(formatTokens(99_999_999), "1亿 tok");
  assert.equal(formatTokens(100_000_000), "1亿 tok");
  assert.equal(formatTokens(125_000_000), "1.3亿 tok");
  assert.equal(formatTokens(1_230_000_000), "12.3亿 tok");
});

test("token settings keep their existing M-token conversion contract", () => {
  assert.equal(tokensToMillions(1_250_000), "1.25");
  assert.equal(millionsToTokens("1.25"), 1_250_000);
});

test("request failures keep a useful message when the server returns plain text or no body", async () => {
  await assert.rejects(responseJson(new Response("Starting up", { status: 503 }), "保存失败"), /保存失败（HTTP 503）/);
  await assert.rejects(responseJson(new Response(JSON.stringify({ error: "输入无效" }), { status: 400 }), "保存失败"), /输入无效/);
  assert.equal(await responseJson(new Response(null, { status: 204 })), null);
  await assert.rejects(responseJson(new Response("broken"), "无法读取状态"), /无法读取状态/);
});
