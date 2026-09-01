import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { annotationLabel, annotationsBySource, attachAnnotations, initInputSignalRelations } from "./input-signal-relations.js";

const source = { id: "a", kind: "external_input" };
const dissent = { id: "d", kind: "attacker_challenge", relatedSignalIds: ["a"], content: "范围过强：论文仅证明局部结果。", sources: [{ url: "https://example.test/paper", detail: "论文" }] };

test("concrete labels describe reviewer findings, not quoted or negated claims", () => {
  assert.equal(annotationLabel(dissent), "过度表述");
  assert.equal(annotationLabel({ content: "将小型基准外推成生产结论。" }), "范围外推");
  assert.equal(annotationLabel({ content: "来源数据存在明显冲突，必须标明口径。" }), "来源冲突");
  assert.equal(annotationLabel({ content: "现有证据并未建立这种联系。" }), "证据不足");
  assert.equal(annotationLabel({ content: "具体依据见论文。", review_reason: "结论明显强于证据。" }), "过度表述");
  assert.equal(annotationLabel({ content: "原文讨论“证据不足”，但这里存在其他问题。" }), "有异议");
  assert.equal(annotationLabel({ content: "这并非过度表述，而是证据不足。" }), "证据不足");
  assert.equal(annotationLabel({ content: "这不是证据不足。需要核对其他问题。" }), "有异议");
  assert.equal(annotationLabel({ content: "请核对论文第二节的定义。" }), "有异议");
});

test("multiple review labels stay compact and keep individual evidence categories", () => {
  const { window } = new JSDOM('<article><div class="message-body">原文</div><footer class="message-actions"></footer></article>');
  const card = window.document.querySelector("article");
  attachAnnotations(card, [dissent, { content: "数据明显冲突。" }, { content: "证据不足。" }],
    (target, entry) => { target.textContent = entry.content; });
  assert.equal(card.querySelector(".input-signal-review-badge").textContent, "△ 过度表述 · 来源冲突 等");
  assert.equal(card.querySelector("summary").textContent, "过度表述 · 来源冲突 等 · 3");
  assert.deepEqual([...card.querySelectorAll(".input-signal-review small")].map(el => el.textContent), ["过度表述", "来源冲突", "证据不足"]);
});

test("old and new dissent annotate only live sources without duplicating repeated records", () => {
  const input = [source, dissent, dissent, { ...dissent, id: "copy" }, { ...dissent, id: "hidden", hidden: true }];
  const before = JSON.stringify(input);
  assert.deepEqual(annotationsBySource(input).get("a").map(s => s.id), ["d"]);
  assert.equal(annotationsBySource([dissent]).size, 0);
  assert.equal(JSON.stringify(input), before);
});

test("annotation projection is idempotent, preserves expansion and removes stale markers", () => {
  const { window } = new JSDOM('<main><article class="input-signal" data-signal-id="a"><div class="message-body">原文</div><footer class="message-actions"></footer></article></main>');
  const main = window.document.querySelector("main");
  let signals = [source, dissent];
  const relations = initInputSignalRelations(main, {
    getSignals: () => signals,
    renderContent: (target, entry) => { target.textContent = entry.content; },
  });
  relations.refresh();
  const marker = main.querySelector("details");
  assert.equal(marker.open, false);
  assert.equal(main.querySelectorAll("article").length, 1);
  assert.equal(main.querySelector(".input-signal-review-badge").textContent, "△ 过度表述");
  assert.equal(marker.querySelector("summary").textContent, "过度表述");
  assert.equal(marker.querySelector("summary").getAttribute("aria-label"), "展开过度表述的具体依据");
  assert.match(marker.textContent, /范围过强/);
  assert.equal(marker.querySelector("a").rel, "noopener noreferrer");
  marker.open = true;
  relations.refresh();
  assert.equal(main.querySelector("details"), marker);
  signals = [source, { ...dissent, content: "更新后的具体依据" }];
  relations.refresh();
  assert.equal(main.querySelector("details").open, true);
  assert.match(main.textContent, /更新后的具体依据/);
  assert.equal(main.querySelector("summary").textContent, "有异议");
  signals = [source];
  relations.refresh();
  assert.equal(main.querySelector("details"), null);
  assert.equal(main.querySelector(".input-signal-review-badge"), null);
});
