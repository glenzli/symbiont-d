# symbiont-d

A small local companion that uses Codex as its reasoning and tool runtime while
keeping its own interface and memory.

The current prototype provides:

- A local chat interface at `http://127.0.0.1:4317`.
- Sanitized Markdown and inline HTML rendering with tables, code blocks, links,
  and KaTeX mathematics.
- Multimodal messages with pasted, dropped, or selected local images.
- A local Paged Context Protocol store with immutable Page revisions, provenance,
  typed relations, and host-enforced scopes.
- Explicit first-run calibration through a pasted description or adaptive
  conversation.
- A visible, editable orientation document separate from conversation memory.
- A background exploration lifecycle with visible status, quiet hours, a daily
  proactive-message limit, and a separate daily autonomous token budget.
- A direct JSON-RPC connection to `codex app-server`.
- A live model catalog and bounded semantic compute lanes.
- Streamed replies and activity states derived from Codex runtime events.
- Per-reply model, reasoning effort, duration, and token metadata.
- A local SQLite usage ledger, including every internal invocation.
- Per-reply PCP call traces with ordered arguments, results, timing, and model
  run boundaries.
- Live Codex web search when the model decides it is useful.
- Client-hosted `pcp` search, read, write, revise, and relation tools.
- Client-hosted `symbiont.complete_orientation` and `symbiont.escalate` tools.

## Requirements

- Rust 1.88 or newer.
- A recent `codex` CLI.
- An existing Codex login (`codex login status`).
- Node.js and npm only when rebuilding the checked-in rich text bundle.

## Run

```bash
cargo run
```

Then open `http://127.0.0.1:4317`.

After changing `web/rich-text-source.js` or its frontend dependencies, rebuild
the browser bundle with:

```bash
npm install
npm run build:web
```

The runtime uses the following local data files:

- `data/context.sqlite3` is the primary PCP context store.
- `data/memory.md` is retained as a non-destructive legacy import source.
- `data/profile.toml` records explicit initialization state.
- `data/orientation.md` is created when calibration completes and contains the
  user-visible and editable working orientation.
- `data/autonomy.toml` records permission, cadence, quiet hours, and autonomous
  usage limits.
- `data/compute.toml` defines the model and reasoning effort for each semantic
  compute lane.
- `data/symbiont.sqlite3` records invocation and token metadata.

Each user and assistant message becomes a source-backed PCP Page. Existing
Markdown entries are imported idempotently at startup and are never overwritten.
Raw conversation history is not injected into every turn. Codex receives only a
small continuity seed containing the authorized Scope names and active
orientation Page reference, then decides when to search and read context through
PCP tools.

The current Store supports exact, FTS text, temporal, and graph retrieval. It
does not collapse those channels into a single relevance score, and it does not
yet use embeddings. The host owns scope authorization, write limits, identity,
visibility, and provenance defaults; the model owns its retrieval strategy and
working set. Source references and provenance remain durable but are cold
projections: ordinary Page reads omit them until the caller explicitly requests
`sources` or `provenance`.

Provenance also forms the derivation DAG. The Store maintains a rebuildable
index of provenance inputs and exposes them to graph search as virtual
`derived_from` edges. Graph queries are one hop and bidirectional, so callers can
move from evidence to derived syntheses or from a synthesis back to its inputs
without storing duplicate Relations. Repeated graph queries traverse deeper
levels. New provenance inputs are deduplicated and must resolve to authorized,
existing Revisions.

New messages use typed content parts while retaining a plain Markdown field for
legacy compatibility. Images are validated, content-addressed by SHA-256, and
stored under `data/assets`; PCP stores an image asset Page and source reference
rather than copying binary data into SQLite. The conversation event links to the
asset through `has_attachment`, and the assistant event links back through
`responds_to`. Internal derivation is recorded once in provenance rather than
duplicated as an automatic `derived_from` relation. The original image remains
available for later visual re-analysis instead of being replaced by a
model-generated caption.

Inline HTML is sanitized before display. Arbitrary scripts and active HTML are
not executed inside messages; future interactive artifacts should use a
separate sandboxed surface.

Normal chat is unavailable until the user explicitly starts calibration.
Autonomous exploration additionally requires a completed orientation and an
enabled autonomy setting. It runs in a separate ephemeral Codex thread so
silent research does not become hidden conversational context. Scheduled runs
respect quiet hours and check the daily message and token limits before
starting. A run can end silently; only a signal the model considers worth
interrupting for becomes an assistant message and PCP conversation Page.

The local usage database stores the ordered PCP tool trace for each invocation.
Assistant message metadata points to the root invocation, so the interface can
open the exact search/read/write chain from a reply. Silent autonomous runs
remain inspectable from the recent invocation list. PCP Revisions actually
observed through those calls are also attached to the resulting message
provenance rather than being inferred later.

Every turn explicitly selects a model and reasoning effort. Ordinary chat starts
in the `conversation` lane. In bounded-auto mode, the model can request a deeper
semantic lane through `symbiont.escalate`; the daemon validates the request and
maps it to a user-configured model.

## Context CLI

The bundled read-only CLI can inspect or export the Store:

```bash
cargo run --bin pcp -- doctor
cargo run --bin pcp -- scopes
cargo run --bin pcp -- search "query" text
cargo run --bin pcp -- read rev_...
cargo run --bin pcp -- export
```

## Configuration

The following environment variables are optional:

- `SYMBIONT_BIND`: listen address, default `127.0.0.1:4317`.
- `SYMBIONT_MEMORY_PATH`: Markdown memory path, default `data/memory.md`.
- `SYMBIONT_PCP_PATH`: PCP database path, default `data/context.sqlite3`.
- `SYMBIONT_ASSET_PATH`: content-addressed image directory, default
  `data/assets`.
- `SYMBIONT_COMPUTE_PATH`: compute configuration path, default
  `data/compute.toml`.
- `SYMBIONT_PROFILE_PATH`: initialization state path, default
  `data/profile.toml`.
- `SYMBIONT_ORIENTATION_PATH`: visible orientation path, default
  `data/orientation.md`.
- `SYMBIONT_AUTONOMY_PATH`: autonomy policy path, default
  `data/autonomy.toml`.
- `SYMBIONT_USAGE_PATH`: usage database path, default
  `data/symbiont.sqlite3`.
- `CODEX_BIN`: Codex executable, default `codex`.

## Architecture

`main.rs` only composes the application. The semantic owners are:

- `pcp-core`: protocol types and host/store request contracts.
- `pcp-sqlite`: SQLite schema, immutable revisions, retrieval, and relations.
- `continuity`: scope policy, source ingestion, migration, and model-facing PCP
  operations.
- `asset`: image validation, content hashing, local persistence, and retrieval.
- `memory`: read-only parsing of the legacy Markdown import source.
- `profile`: explicit initialization state and the visible orientation.
- `autonomy`: durable permission and operational boundaries for exploration.
- `exploration`: scheduled admission, runtime state, budget gates, and
  publication of proactive messages.
- `codex`: the app-server process, protocol lifecycle, and dynamic tool bridge.
- `compute`: the live model catalog and durable semantic lane policy.
- `usage`: the SQLite invocation ledger, PCP tool traces, and aggregate
  statistics.
- `web`: the HTTP API and embedded chat surface.

The app-server `dynamicTools` field is currently experimental. The client opts
into the experimental protocol during initialization and fails clearly if the
installed Codex version does not support it.
