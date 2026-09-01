import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const shell = await readFile(new URL("./index.html", import.meta.url), "utf8");

test("places the Topic entry with the floating conversation controls", () => {
  const floatingStart = shell.indexOf('id="conversation-floating-controls"');
  const floatingEnd = shell.indexOf("</div>", floatingStart);
  const topicEntry = shell.indexOf('id="open-topics"');
  const topbarEnd = shell.indexOf("</header>");

  assert.ok(floatingStart >= 0);
  assert.ok(topicEntry > floatingStart && topicEntry < floatingEnd);
  assert.ok(topicEntry > topbarEnd);
});

test("keeps active Topic context in the Topic chat header instead of the composer", () => {
  assert.doesNotMatch(shell, /id="topic-target-tray"/);
  assert.match(shell, /id="topic-chat-title"/);
  assert.match(shell, /id="exit-topic-chat"/);
});
