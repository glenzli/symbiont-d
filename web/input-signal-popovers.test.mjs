import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { initSignalPopovers } from "./input-signal-popovers.js";

function fixture() {
  const window = new JSDOM(`<main><article>
    <details data-signal-popover><summary tabindex="0">旧摘要</summary><div class="signal-popover-panel"><a href="#evidence">来源</a></div></details>
    <details data-signal-popover><summary tabindex="0">说明</summary><p class="signal-popover-panel">依据</p></details>
    </article><article><details data-signal-popover><summary tabindex="0">过度表述</summary><div class="signal-popover-panel">反驳</div></details></article>
    <details id="unrelated"><summary>其他内容</summary><p>无关</p></details><button id="blank">空白区</button></main>`).window;
  const doc = window.document;
  const controls = [...doc.querySelectorAll("[data-signal-popover]")];
  const controller = initSignalPopovers(doc);
  const click = i => controls[i].querySelector("summary").click();
  const open = () => controls.filter(el => el.open);
  return { window, doc, controls, controller, click, open };
}

test("source, note and dissent panels are exclusive across cards and toggle closed", () => {
  const { window, doc, controls, controller, click, open } = fixture();
  assert.equal(initSignalPopovers(doc), controller);
  click(0);
  assert.deepEqual(open(), [controls[0]]);
  click(1);
  assert.deepEqual(open(), [controls[1]]);
  click(2);
  assert.deepEqual(open(), [controls[2]]);
  click(2);
  assert.equal(open().length, 0);
  controller.destroy();
  window.close();
});

test("inside clicks preserve evidence; outside clicks dismiss without changing unrelated details", () => {
  const { window, doc, controls, controller, click, open } = fixture();
  doc.querySelector("#unrelated").open = true;
  click(0);
  controls[0].querySelector("a").click();
  assert.equal(open().length, 1);
  doc.querySelector("#blank").click();
  assert.equal(open().length, 0);
  assert.equal(doc.querySelector("#unrelated").open, true);
  controller.destroy();
  window.close();
});

test("Escape closes only the active panel and restores its trigger focus", () => {
  const { window, doc, controls, controller, click, open } = fixture();
  click(0);
  controls[0].querySelector("a").focus();
  const escape = new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  doc.activeElement.dispatchEvent(escape);
  assert.equal(open().length, 0);
  assert.equal(doc.activeElement, controls[0].querySelector("summary"));
  assert.equal(escape.defaultPrevented, true);
  controller.destroy();
  window.close();
});

test("panel scrolling stays open, anchor scrolling and resize dismiss it", () => {
  const { window, doc, controls, controller, click, open } = fixture();
  click(0);
  controls[0].querySelector(".signal-popover-panel").dispatchEvent(new window.Event("scroll"));
  assert.equal(open().length, 1);
  doc.querySelector("main").dispatchEvent(new window.Event("scroll"));
  assert.equal(open().length, 0);
  click(0);
  window.dispatchEvent(new window.Event("resize"));
  assert.equal(open().length, 0);
  controller.destroy();
  window.close();
});

test("panels near the lower-right corner fit inside the viewport and open upward", () => {
  const { window, controls, controller, click } = fixture();
  window.innerWidth = 360;
  window.innerHeight = 500;
  controls[0].querySelector("summary").getBoundingClientRect = () => ({ left: 310, top: 450, bottom: 475 });
  const panel = controls[0].querySelector(".signal-popover-panel");
  panel.getBoundingClientRect = () => ({ width: Number.parseFloat(panel.style.width), height: 200 });
  click(0);
  assert.equal(panel.style.width, "336px");
  assert.equal(panel.style.left, "12px");
  assert.equal(panel.style.top, "244px");
  assert.equal(panel.style.maxHeight, "432px");
  controller.destroy();
  window.close();
});

test("programmatic reopening and replacement use the same mutual-exclusion lifecycle", async () => {
  const { window, doc, controls, controller, click, open } = fixture();
  click(0);
  controls[1].open = true;
  await new Promise(resolve => window.setTimeout(resolve, 10));
  assert.deepEqual(open(), [controls[1]]);
  controls[1].remove();
  const next = controls[2].cloneNode(true);
  doc.querySelector("main").append(next);
  next.querySelector("summary").click();
  assert.equal(next.open, true);
  doc.querySelector("#blank").click();
  assert.equal(next.open, false);
  controller.destroy();
  window.close();
});
