import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { connectionIssues, initTopbarUi } from "./topbar-ui.js";

test("only enabled connection channels surface actionable issues", () => {
  assert.deepEqual(connectionIssues({
    driveInput: { enabled: false, lastError: "refresh expired", oauth: { status: "invalid" } },
    mailInput: { enabled: true, availability: "ready", lastError: null },
  }), []);

  const [issue] = connectionIssues({
    driveInput: {
      enabled: true,
      availability: "ready",
      lastError: "refresh personal Google Drive authorization",
      oauth: { status: "connected" },
    },
  });
  assert.equal(issue.label, "Drive 需处理");
  assert.equal(issue.settingsTab, "sources");
  assert.equal(issue.sourceTab, "drive");
});

test("topbar alert stays visible and opens the affected source settings", () => {
  const dom = new JSDOM(`<details id="top-overflow"></details>
    <button id="connection-alert" hidden><span id="connection-alert-label"></span><strong id="connection-alert-count"></strong></button>`);
  const prior = { document: globalThis.document };
  globalThis.document = dom.window.document;
  try {
    const state = {
      driveInput: { enabled: true, availability: "missing_credential", oauth: { status: "disconnected" } },
      mailInput: { enabled: true, availability: "credential_unavailable" },
    };
    const opened = [];
    const ui = initTopbarUi(state, { openSettings: (...route) => opened.push(route) });
    ui.render();
    const alert = document.querySelector("#connection-alert");
    assert.equal(alert.hidden, false);
    assert.equal(document.querySelector("#connection-alert-label").textContent, "Drive、邮箱需处理");
    assert.equal(document.querySelector("#connection-alert-count").hidden, true);
    assert.match(alert.getAttribute("aria-label"), /打开设置处理/);
    alert.click();
    assert.deepEqual(opened, [["sources", "drive"]]);
  } finally {
    globalThis.document = prior.document;
    dom.window.close();
  }
});
