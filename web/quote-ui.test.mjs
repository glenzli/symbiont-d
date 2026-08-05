import assert from "node:assert/strict";
import test from "node:test";

import { quoteDraft } from "./quote-ui.js";

test("converts a rendered quote back into the outgoing draft contract", () => {
  assert.deepEqual(
    quoteDraft({
      sourceRevisionId: "rev_source",
      sourceRole: "assistant",
      sourceAt: "2026-08-06T04:35:00Z",
      text: "The stored excerpt must survive retry.",
      sourceSha256: "ignored-on-submit",
      startOffset: 10,
      endOffset: 47,
      wholeMessage: false,
      truncated: false,
    }),
    {
      sourceRevisionId: "rev_source",
      selectedText: "The stored excerpt must survive retry.",
      startOffset: 10,
      endOffset: 47,
      wholeMessage: false,
    },
  );
});

test("preserves a new quote draft and unwraps message parts", () => {
  assert.deepEqual(
    quoteDraft({
      type: "quote",
      quote: {
        sourceRevisionId: "rev_source",
        selectedText: "A freshly selected excerpt.",
        startOffset: null,
        endOffset: null,
        wholeMessage: true,
      },
    }),
    {
      sourceRevisionId: "rev_source",
      selectedText: "A freshly selected excerpt.",
      startOffset: null,
      endOffset: null,
      wholeMessage: true,
    },
  );
});
