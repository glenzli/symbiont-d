import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { signalContent, appendSignalDetails } from "./input-signal-content.js";

const original = "### 第一篇论文\n条件 $a > 0$ 下，证明 $f(a)=a^2$。\n\n### 第二篇论文\n给出反例，不适用于全部模型。";
const legacy = { presentation: "condensed", content: "该摘要汇总两篇论文。", receivedText: original };

test("legacy summaries no longer hide source results, assumptions, formulas or links", () => {
  assert.equal(signalContent(legacy).text, original);
  assert.deepEqual(signalContent(legacy).alternative, { label: "旧摘要", text: legacy.content });
  assert.equal(legacy.content, "该摘要汇总两篇论文。");
  assert.equal(signalContent({ ...legacy, receivedText: undefined, received_text: original }).text, original);
});

test("deterministic duplicate excerpts stay filtered, with complete source available on request", () => {
  const excerpt = { ...legacy, presentation: "excerpted", content: "### 第二篇论文\n给出反例，不适用于全部模型。" };
  assert.equal(signalContent(excerpt).text, excerpt.content);
  assert.deepEqual(signalContent(excerpt).alternative, { label: "完整原文", text: original });
  assert.ok(!signalContent(excerpt).text.includes("第一篇"));
});

test("missing source and ordinary inputs retain their existing content safely", () => {
  assert.equal(signalContent({ presentation: "condensed", content: "仅剩摘要" }).text, "仅剩摘要");
  assert.equal(signalContent({ content: "正文", receivedText: "原文" }).text, "正文");
  assert.deepEqual(signalContent({}), { text: "", alternative: null });
  assert.equal(signalContent({ ...legacy, content: original }).alternative, null);
});

test("shared details preserve qualification, compact transport labels and safe source links", () => {
  const doc = new JSDOM("<footer></footer>").window.document;
  appendSignalDetails(doc.querySelector("footer"), {
    ...legacy,
    qualificationNote: "未核验；原文中的断言不代表审阅结论。",
    sources: [
      { url: "https://arxiv.org/abs/1234.56789", detail: "Linked through the user-configured Google Drive Inbox" },
      { url: "javascript:alert(1)", detail: "不安全链接" },
    ],
  }, (target, entry) => { target.textContent = entry.content; });
  assert.deepEqual([...doc.querySelectorAll("summary")].map(el => el.textContent), ["旧摘要", "说明", "1 个来源"]);
  assert.equal(doc.querySelectorAll("details[data-signal-popover] > .signal-popover-panel").length, 3);
  assert.match(doc.querySelector(".input-signal-qualification").textContent, /未核验/);
  assert.equal(doc.querySelector("a").textContent, "arxiv.org/abs/1234.56789");
  assert.equal(doc.querySelector("a").rel, "noopener noreferrer");
  assert.match(doc.querySelector("a").title, /Google Drive/);
});
