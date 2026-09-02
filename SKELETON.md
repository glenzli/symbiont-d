# Source ownership

This is a routing map for maintained source boundaries, not a product roadmap.

## Model context and inspection

- `src/context_assembly.rs`: typed context fragments, provenance/omission audit and final optional-recall budget. Audit metadata is not sent to models.
- `src/web.rs`: foreground composition uses identity/boundary, selected route and federated recall; background maps, hypotheses and queues are deferred. `src/reflection/worker.rs`, `src/context_maintenance.rs` and `src/exploration.rs` select their own task-specific background inputs.
- `src/continuity/compound.rs`: compact PCP and local-source evidence with exact identities, source resolution and unavailable-versus-miss semantics; no rewriting of stored content.
- `src/codex/client.rs` and `src/codex/prompts.rs`: submit the selected fragments and capture the actual thread configuration and turn request. Native/provider final prompts are not exposed. Conversation tool registration is separate from maintenance tools.
- `web/context-inspector.js`: source-attributed input inspection, exact client-request export and explicit historical-record limitations; embedded through `src/web.rs`.

## Memory ownership and retirement

- `src/reflection/worker.rs`: conversation-driven topic/hypothesis review, recurrence evidence and autonomous retention decisions. `src/context_maintenance.rs` maintains local working context; neither owns PCP library maintenance.
- `src/continuity.rs` and `src/codex/tools.rs`: tenant recall, source resolution, autonomous ingest and exact-Revision feedback. Runtime owns PCP semantic projections and governance.
- `src/continuity/retention.rs`: shared autonomous write preflight, exact-source/current-head review tokens, temporal attribution, and restart-safe deferred proposals/receipts. `GET /api/retention` exposes unsaved proposals; Reflection resumes them only after retrieval recovers.
- `src/retired_memory.rs`: state-free HTTP 410 responses for retired reconciliation actions. The old UI, worker, Summary loop and episode-index sync are removed. Existing `reconciliation.json` and usage/trace records are not migrated, rewritten or deleted.
- `src/bin/symbiont-pcp-worker.rs`: legacy command compatibility only; returns protocol `defer` locally without network/model calls. Operators should configure maintenance in PCP Runtime.

## External inputs

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
