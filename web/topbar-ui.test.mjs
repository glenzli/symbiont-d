import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";
import { connectionIssues, initTopbarUi } from "./topbar-ui.js";

test("one transient poll failure or failed optional OAuth reconnect does not raise an alert", () => {
  assert.deepEqual(connectionIssues({
    driveInput: { enabled: false, lastError: "refresh expired", oauth: { status: "invalid" } },
    mailInput: { enabled: true, availability: "ready", lastError: null },
  }), []);

  assert.deepEqual(connectionIssues({
    driveInput: { enabled: true, availability: "ready", lastError: "temporary network error", consecutivePollFailures: 1,
      oauth: { status: "failed", error: "optional reconnect failed" } },
    mailInput: { enabled: true, availability: "ready", lastError: "temporary network error", consecutivePollFailures: 1 },
  }), []);
});

test("repeated polling failures and unavailable credentials remain visible", () => {
  const [issue] = connectionIssues({
    driveInput: {
      enabled: true,
      availability: "ready",
      lastError: "refresh personal Google Drive authorization",
      consecutivePollFailures: 2,
      oauth: { status: "connected" },
    },
  });
  assert.equal(issue.label, "Drive 连续读取失败");
  assert.equal(issue.settingsTab, "sources");
  assert.equal(issue.sourceTab, "drive");
  assert.equal(connectionIssues({ mailInput: { enabled: true, availability: "missing_credential" } })[0].label,
    "邮箱未连接");
  assert.equal(connectionIssues({ driveInput: { enabled: true, availability: "ready",
    lastError: "invalid_grant", consecutivePollFailures: 1, requiresReauthorization: true } })[0].label,
  "Drive 授权失效");
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
    assert.equal(document.querySelector("#connection-alert-label").textContent, "Drive、邮箱连接异常");
    assert.equal(document.querySelector("#connection-alert-count").hidden, true);
    assert.match(alert.getAttribute("aria-label"), /打开设置处理/);
    alert.click();
    assert.deepEqual(opened, [["sources", "drive"]]);
  } finally {
    globalThis.document = prior.document;
    dom.window.close();
  }
});
