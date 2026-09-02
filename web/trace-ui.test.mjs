import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { JSDOM } from "jsdom";

test("served trace UI reports automatic input separately from zero model tool calls", async () => {
  const web = path.dirname(fileURLToPath(import.meta.url));
  const built = await build({ entryPoints: [path.join(web, "trace-ui.js")], bundle: true,
    write: false, format: "esm", plugins: [{ name: "served-modules", setup(builder) {
      builder.onResolve({ filter: /^\// }, ({ path: url, kind }) => kind === "entry-point" ? undefined : { path: path.join(web, url) });
    } }], logLevel: "silent" });
  const dom = new JSDOM('<button class="trace-button" data-trace-id="trace"></button><dialog id="trace-dialog"><p id="trace-summary"></p><main id="trace-content"></main></dialog>');
  const previous = { document: globalThis.document, Element: globalThis.Element, fetch: globalThis.fetch };
  try {
    globalThis.document = dom.window.document;
    globalThis.Element = dom.window.Element;
    document.querySelector("dialog").showModal = () => {};
    globalThis.fetch = async () => ({ ok: true, json: async () => ({
      pcpRecallCalls: 0, pcpWriteCalls: 0, eventCount: 0, detailsRetained: true,
      retentionDays: 7, retentionInvocations: 128, runs: [{ invocationId: "run", model: "model", steps: [], events: [],
        context: { fragments: [{ source: "symbiont.pcp.rev_1", value: '{"detail":"payload","content":"记忆"}' }], input: [] },
      }],
    }) });
    const ui = await import(`data:text/javascript;base64,${Buffer.from(built.outputFiles[0].text).toString("base64")}`);
    ui.initTraceUi();
    document.querySelector("button").click();
    await new Promise(resolve => setImmediate(resolve));
    assert.match(document.querySelector("#trace-summary").textContent, /自动装入 1 条/);
    assert.match(document.querySelector("#trace-summary").textContent, /模型追加 PCP 检索 0 次/);
    assert.match(document.querySelector("#trace-content").textContent, /PCP 1 条正文/);
    const projected = { success: true, contentItems: [{ text: '{"content":"完整正文"}' }],
      _symbiontTrace: { rawResult: { manifest: "重复的原始包装" }, invokedVia: "symbiont.invoke_tool" } };
    assert.deepEqual(ui.modelToolResult(projected), { success: true, contentItems: projected.contentItems });
    assert.equal(projected._symbiontTrace.rawResult.manifest, "重复的原始包装");
  } finally {
    Object.assign(globalThis, previous);
    dom.window.close();
  }
});

test("retention groups exact proposals and counts committed receipts, not prechecks", async () => {
  const web = path.dirname(fileURLToPath(import.meta.url));
  const built = await build({ entryPoints: [path.join(web, "trace-ui.js")], bundle: true,
    write: false, format: "esm", plugins: [{ name: "served-modules", setup(builder) {
      builder.onResolve({ filter: /^\// }, ({ path: url, kind }) => kind === "entry-point" ? undefined : { path: path.join(web, url) });
    } }], logLevel: "silent" });
  const dom = new JSDOM('<button class="trace-button" data-trace-id="trace"></button><dialog id="trace-dialog"><p id="trace-summary"></p><main id="trace-content"></main></dialog>');
  const previous = { document: globalThis.document, Element: globalThis.Element, fetch: globalThis.fetch };
  const step = (sequence, proposalId, status, created = false, extra = {}) => ({
    sequence, namespace: "pcp", tool: "write_page", succeeded: true, durationMs: 12,
    arguments: { content: "不以相似正文合并不同提案" },
    result: { contentItems: [{ text: JSON.stringify({ proposalId, status, created, ...extra }) }] },
  });
  try {
    globalThis.document = dom.window.document;
    globalThis.Element = dom.window.Element;
    document.querySelector("dialog").showModal = () => {};
    let steps = [step(0, "retain_a", "review_required"), step(1, "retain_b", "review_required"),
      step(2, "retain_a", "written", true), step(3, "retain_b", "discarded"),
      step(4, "retain_a", "written", false, { reusedReceipt: true })];
    globalThis.fetch = async () => ({ ok: true, json: async () => ({
      pcpRecallCalls: 0, eventCount: 5, detailsRetained: true, retentionDays: 7, retentionInvocations: 128,
      runs: [{ invocationId: "run", model: "model", steps,
        events: steps.map(step => ({ sequence: step.sequence, kind: "toolCall", details: { toolSequence: step.sequence } })) }],
    }) });
    const ui = await import(`data:text/javascript;base64,${Buffer.from(built.outputFiles[0].text).toString("base64")}`);
    ui.initTraceUi();
    async function open() { document.querySelector("button").click(); await new Promise(resolve => setImmediate(resolve)); }
    await open();
    const groups = document.querySelectorAll(".trace-retention-process");
    assert.equal(groups.length, 2);
    assert.match(groups[0].querySelector("summary").textContent, /PCP 留存 · 已写入/);
    assert.match(groups[1].querySelector("summary").textContent, /仅保留本地聊天/);
    assert.equal(groups[0].querySelectorAll(".trace-step").length, 3);
    assert.match(groups[0].textContent, /写入预检（未写入）/);
    assert.match(groups[0].textContent, /复用已有回执（未再次写入）/);
    assert.match(document.querySelector("#trace-summary").textContent, /1 次写入/);
    assert.equal(groups[1].querySelectorAll('[data-pcp-action="write"]').length, 0);
    assert.match(groups[0].textContent, /模型收到的输出/);

    steps = [step(0, "retain_c", "review_required"), step(1, "retain_d", "covered"),
      step(2, "retain_e", "deferred"), step(3, "retain_f", "written", false)];
    await open();
    assert.match(document.querySelector("#trace-summary").textContent, /0 次写入/);
    assert.match(document.querySelector("#trace-content").textContent, /已有内容覆盖（未写入）/);
    steps = [{ ...step(0, "lost", "written", true), result: { contentItems: [{ text: "[truncated]" }] } }];
    await open();
    assert.match(document.querySelector("#trace-summary").textContent, /0 次写入/);
    assert.match(document.querySelector("#trace-content").textContent, /结果未确认/);
  } finally {
    Object.assign(globalThis, previous);
    dom.window.close();
  }
});
