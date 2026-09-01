# Source ownership

This is a routing map for the external-input path, not a product roadmap.

- `src/drive_input.rs`: read-only Drive listing, oldest-first file selection, persistent acknowledgement IDs. Document time is separate from intake time and event time.
- `src/external_digest.rs` and `src/external_markdown.rs`: shared document sectioning, provenance and transport normalization.
- `src/signals.rs`: local signal lifecycle, source/annotation relationships and visible source windows.
- `src/inference/sensing_review.rs` and `src/exploration/sensing_route.rs`: admission and routing preserve received text; caveats do not replace it. Deterministic duplicate-section removal is explicitly `excerpted`, distinct from legacy model `condensed` summaries.
- `src/signals/dedup.rs`: section-level delivery evidence independent of UI retention (180 days, up to 4,096 references). This is not PCP memory.
- `src/inference/sensing_similarity.rs`: candidate-specific lexical/source and local-vector retrieval, exact embedding-space validation and padded batch budget. Similarity is not a suppression verdict.
- `src/inference/sensing_duplicate.rs`: deterministic delivery identity and conservative semantic-verdict contract. New evidence and changed results remain eligible.
- `web/markdown-renderer.mjs`: MarkdownIt + TeX grammar, KaTeX and the sanitized DOM boundary; `rich-text-source.js` composes message parts. Rebuild `rich-text.js` after renderer changes.
- `web/input-signal-relations.js`: source-attached review annotations, shared by conversation and briefing; historical challenge records use the same projection without data rewriting.
- `web/input-signal-content.js`: source-first body projection and shared source/qualification details for conversation and briefing; legacy summaries remain optional, duplicate excerpts remain filtered.
- `web/input-signal-popovers.js`: exclusive signal-detail/annotation panels, light dismissal, keyboard focus and viewport placement across both views.
- `web/message-sync.js`: arrival-based unread state and viewport read tracking. Historical backfill is new delivery; annotations are not independent unread items.

## Focused verification

Run `npm run test:web` and `npm run build:rich-text` for rendering/UI changes. Backend regression owners are `signals::`, `external_digest::`, `external_markdown::`, `drive_input::`, `sensing::`, `inference::sensing_`, and `attacker::`. UI evidence must use the rebuilt assets; unit tests alone do not prove the running menu window has reloaded.
