import assert from "node:assert/strict";
import test from "node:test";

import {
  createTopicExpansion,
  isMessageExpanded,
  topicMessageKey,
} from "./topic-expansion.js";
import {
  messageBelongsToTopic,
  topicChatMessageKey,
} from "./topic-chat.js";

test("a newly opened topic pack expands its conversation by default", () => {
  const state = createTopicExpansion();

  assert.equal(isMessageExpanded(state, "msg_1"), true);
});

test("an explicitly collapsed message remains collapsed while its pack stays open", () => {
  const state = createTopicExpansion();
  state.collapsed.add("msg_1");

  assert.equal(isMessageExpanded(state, "msg_1"), false);
  assert.equal(isMessageExpanded(state, "msg_2"), true);
});

test("message expansion keys prefer durable transcript identities", () => {
  assert.equal(
    topicMessageKey({ revisionId: "msg_durable", role: "assistant", at: "2026-08-20" }),
    "msg_durable",
  );
  assert.match(
    topicMessageKey({ role: "assistant", at: "2026-08-20", content: "fallback" }, 2),
    /^assistant:2026-08-20:2:fallback$/,
  );
});

test("topic chat projects only messages carrying the active topic reference", () => {
  const message = {
    revisionId: "msg_topic",
    parts: [{ type: "topic", topic: { topicId: "ep_active", title: "Active" } }],
  };

  assert.equal(messageBelongsToTopic(message, "ep_active"), true);
  assert.equal(messageBelongsToTopic(message, "ep_other"), false);
  assert.equal(messageBelongsToTopic({ parts: [] }, "ep_active"), false);
});

test("topic chat deduplication uses the canonical transcript revision", () => {
  assert.equal(
    topicChatMessageKey({ revisionId: "msg_durable", content: "same transcript" }),
    "msg_durable",
  );
});
