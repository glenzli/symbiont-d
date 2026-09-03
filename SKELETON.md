# Source ownership

This is a routing map for maintained source boundaries, not a product roadmap.

## Model context and inspection

- `src/context_assembly.rs`: typed context fragments, provenance/omission audit and final optional-recall budget. Audit metadata is not sent to models.
- `src/web.rs`: foreground composition uses identity/boundary, selected route and federated recall; background maps, hypotheses and queues are deferred. `src/reflection/worker.rs`, `src/context_maintenance.rs` and `src/exploration.rs` select their own task-specific background inputs.
- `src/continuity/compound.rs`: compact PCP and local-source evidence with exact identities, source resolution and unavailable-versus-miss semantics; no rewriting of stored content.
- `src/continuity/recall_selection.rs`: foreground-only joint relevance admission through local Infer reranking, conservative lexical fallback, identity-bound source coverage, and diagnostic-only selection evidence. Stored content and background recurrence remain independent.
- `src/continuity/recall_sources.rs`: bounded, read-only Revision-to-SourceRef lineage expansion. Missing, foreign, denied or cyclic sources are not coverage proof; this reader neither performs writes nor caches ACL decisions.
- `src/continuity/context_inbox.rs`: optional PCP Runtime Context Inbox candidate/activity staging through the typed tenant client, with rolling support for the former capability name. Candidates and cards stay outside formal Page recall; stable local evidence identities and the target write Scope remain host-owned, while cross-Scope derivation evidence is preserved for Runtime ACL enforcement.
- `src/codex/client.rs` and `src/codex/prompts.rs`: submit the selected fragments and capture the actual thread configuration and turn request. Native/provider final prompts are not exposed. Conversation tool registration is separate from maintenance tools.
- `src/codex/tool_surface.rs`: minimal stage registration, exact-schema discovery, deferred call normalization and origin allowlists; execution remains behind domain/evidence guards. It consumes the host-owned catalog, not the PCP MCP catalog.
- `src/codex/pcp_projection.rs`: consumes `pcp-client::model_context` directly, without MCP or double clipping; adds explicit storage-actor attribution. Source/history/full are evidence views, raw API objects remain diagnostic-only. Retention preflight uses the same projection without changing source/token identity.
- `src/exploration/context.rs` and `src/codex/exploration_context.rs`: separate exploration intent, user hints, unverified candidates and negative delivery ledger; reviewer admission uses selected anchors instead of replaying the scout bundle.
- `web/context-inspector.js`: source-attributed input inspection, exact client-request export and explicit historical-record limitations; embedded through `src/web.rs`.

## Memory ownership and retirement

- `src/reflection/worker.rs`: conversation-driven topic/hypothesis review, recurrence evidence and autonomous retention decisions. `src/context_maintenance.rs` maintains local working context; neither owns PCP library maintenance.
- `src/continuity.rs` and `src/codex/tools.rs`: tenant recall, source resolution, autonomous ingest and exact-Revision feedback. Runtime owns PCP semantic projections and governance.
- `src/continuity/retention.rs`: shared autonomous write preflight, separate novelty/recall-value review, exact-source/current-head tokens, temporal attribution, and restart-safe deferred proposals/receipts. Weak material remains in local chat, not a periodic retry. `GET /api/retention` exposes unsaved proposals; Reflection resumes them only after retrieval recovers. `web/trace-ui.js` groups receipt-linked calls as one retention process and distinguishes precheck from an actual write.
- `src/retired_memory.rs`: state-free HTTP 410 responses for retired reconciliation actions. The old UI, worker, Summary loop and episode-index sync are removed. Existing `reconciliation.json` and usage/trace records are not migrated, rewritten or deleted.
- `src/bin/symbiont-pcp-worker.rs`: legacy command compatibility only; returns protocol `defer` locally without network/model calls. Operators should configure maintenance in PCP Runtime.

## External inputs

- `src/drive_input.rs`: read-only Drive listing, oldest-first file selection, persistent acknowledgement IDs. Document time is separate from intake time and event time.
- `src/external_digest.rs` and `src/external_markdown.rs`: shared document sectioning, provenance and transport normalization.
- `src/signals.rs`: local signal lifecycle, source/annotation relationships and visible source windows. Reply provenance is copied into the local transcript; replying no longer promotes raw input into PCP.
- `src/inference/sensing_review.rs` and `src/exploration/sensing_route.rs`: admission and routing preserve received text; caveats do not replace it. Deterministic duplicate-section removal is explicitly `excerpted`, distinct from legacy model `condensed` summaries.
- `src/signals/dedup.rs`: section-level delivery evidence independent of UI retention (180 days, up to 4,096 references). This is not PCP memory.
- `src/inference/sensing_similarity.rs`: candidate-specific lexical/source and local-vector retrieval, exact embedding-space validation and padded batch budget. Similarity is not a suppression verdict.
- `src/inference/sensing_duplicate.rs`: deterministic delivery identity and conservative semantic-verdict contract. New evidence and changed results remain eligible.
- `web/markdown-renderer.mjs`: MarkdownIt + TeX grammar, KaTeX and the sanitized DOM boundary; `rich-text-source.js` composes message parts. Rebuild `rich-text.js` after renderer changes.
- `web/input-signal-relations.js`: source-attached review annotations, shared by conversation and briefing; historical challenge records use the same projection without data rewriting.
- `web/input-signal-content.js`: source-first body projection and shared source/qualification details for conversation and briefing; legacy summaries remain optional, duplicate excerpts remain filtered.
- `web/input-signal-popovers.js`: exclusive signal-detail/annotation panels, light dismissal, keyboard focus and viewport placement across both views.
- `web/message-sync.js`: arrival-based unread state and viewport read tracking. Historical backfill is new delivery; annotations are not independent unread items.
- `web/signal-reply-ui.js`: one-message external reply selection shared by timeline and briefing, visible cancellable draft, empty-send feedback and generation-bound failure restoration. `app.js` passes the selected signal explicitly at submission; it is not ambient context.

## Focused verification

Run `npm run test:web` and `npm run build:rich-text` for rendering/UI changes. Backend regression owners are `signals::`, `external_digest::`, `external_markdown::`, `drive_input::`, `sensing::`, `inference::sensing_`, and `attacker::`. UI evidence must use the rebuilt assets; unit tests alone do not prove the running menu window has reloaded.
