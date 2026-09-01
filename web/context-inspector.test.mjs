import test from "node:test";
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { renderContextInspector, submittedContextExport } from "./context-inspector.js";

function context() {
  const content = "原始中文 $f^*$ <script>danger()</script>";
  const fragment = { source: "symbiont.transcript.msg_1", kind: "application", value: content };
  return {
    input: [{ type: "text", text: "当前问题" }], fragments: [fragment], developerInstructions: "权限不可扩大",
    nativeThread: { threadId: "thread-1", priorTurns: 0, compactionsBefore: 0, observableHistoryTail: [], exactPromptAvailable: false },
    selection: [
      { source: fragment.source, origin: "本地聊天记录", purpose: "查询匹配", included: true, chars: 35 },
      { source: "symbiont.background.reflection", origin: "后台记录", purpose: "本轮不需要", included: false, chars: 0 },
    ],
    submitted: {
      threadStart: { developerInstructions: "权限不可扩大", dynamicTools: [{ name: "pcp", tools: [{ name: "write_page" }] }] },
      turnStart: { threadId: "thread-1", input: [{ type: "text", text: "当前问题" }], additionalContext: { [fragment.source]: { kind: fragment.kind, value: fragment.value } } },
    },
  };
}

test("source inspector distinguishes sent material, deferred background and opaque native context", () => {
  const { window } = new JSDOM("<main></main>");
  const view = renderContextInspector(context(), window.document);
  window.document.querySelector("main").append(view);
  assert.match(view.textContent, /本地聊天记录/);
  assert.match(view.textContent, /本轮未装入/);
  assert.match(view.textContent, /不能称为模型的完整最终提示词/);
  const raw = view.querySelector(".context-source details");
  assert.equal(raw.querySelector("pre"), null);
  raw.open = true;
  raw.dispatchEvent(new window.Event("toggle"));
  assert.equal(raw.querySelector("pre").textContent, context().fragments[0].value);
  assert.equal(view.querySelector("script"), null);
});

test("complete export preserves exact request, tool definitions and long input without confusing audit with input", () => {
  const value = context();
  value.submitted.turnStart.input[0].text = "原文".repeat(60_000);
  const exported = JSON.parse(submittedContextExport(value));
  assert.deepEqual(exported.turnStart, value.submitted.turnStart);
  assert.deepEqual(exported.threadStart, value.submitted.threadStart);
  assert.equal(exported.diagnosticSelectionNotSentToModel.length, 2);
  assert.equal(exported.turnStart.additionalContext["symbiont.background.reflection"], undefined);
});

test("copy affordance copies the exact complete export and reports success", async () => {
  const { window } = new JSDOM("<main></main>");
  let copied;
  Object.defineProperty(window.navigator, "clipboard", { value: { writeText: async text => { copied = text; } } });
  const value = context();
  const view = renderContextInspector(value, window.document);
  view.querySelector("button").click();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(copied, submittedContextExport(value));
  assert.match(view.querySelector("[role=status]").textContent, /已复制/);
});

test("legacy snapshots remain readable without fabricated complete requests", () => {
  const value = context();
  delete value.submitted;
  delete value.selection;
  const { window } = new JSDOM("");
  const view = renderContextInspector(value, window.document);
  assert.equal(submittedContextExport(value), null);
  assert.match(view.textContent, /旧轨迹没有保存完整请求/);
  assert.equal(view.querySelector("button"), null);
});
