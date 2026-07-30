# symbiont-d

`symbiont-d` is a local, persistent information companion built on the Codex
runtime.

It is not a feed reader or a conventional recommendation system. Its job is to
keep enough awareness of the outside world to bring useful questions, evidence,
and unexpected connections into an ongoing conversation, without turning the
user's attention into a feed.

> This repository is an early working prototype. Memory behavior, autonomous
> exploration, and the Codex integration are still evolving.

## Current Prototype

- Local chat interface with streaming responses.
- Continuous message bursts can join an in-flight response; a superseded draft
  is reconsidered against the newest message before publication.
- Latest-message recall, edit-as-resend, and recovery for interrupted sends.
- Direct connection to `codex app-server`; no separate model API pipeline.
- Explicit `$symbiont` skill for bringing a bounded live context packet into
  any Codex task without starting another model run.
- Opt-in Codex task browser: listing reads metadata only, and task content is
  fetched only after a user selects it.
- Live model catalog and configurable compute lanes.
- Markdown, sanitized inline HTML, KaTeX, code blocks, tables, and images.
- Local [Paged Context Protocol](https://glenzli.com/projects/paged-context-protocol/)
  store with immutable revisions and provenance.
- Optional model-written Summary entries for routing to longer Detail.
- Current Map, Open Loops, and a cautious long-term profile review.
- A separate Reflection pipeline that records interaction facts, then uses a
  model to maintain temporal Episodes, revisable hypotheses, and optional
  delayed follow-ups.
- Exact local time is injected into every model run; reply delay, message
  length, continuation, correction, and read state remain contextual evidence,
  never numeric ratings.
- Model-owned Hunches collected in a visible Curiosity Map.
- Scheduled, manual, and conversation-triggered autonomous exploration.
- Silent exploration when there is nothing worth interrupting for.
- Token accounting, daily autonomous limits, recent exploration history, and
  inspectable execution traces.

## How It Fits Together

```text
chat / timer / hunch
        |
        v
symbiont-d host
  |-- Codex app-server: reasoning, web search, model routing
  |-- PCP store: messages, summaries, provenance, long-term Pages
  |-- Symbiont Context: current map, open loops, profile review
  |-- Reflection: raw interaction events -> Episodes -> working hypotheses
  |-- Curiosity: model-owned questions and their lifecycle
  `-- local UI: conversation, settings, archive, usage, traces
```

The Codex thread provides short-term conversational flow. PCP is the durable
context boundary. Older information is searched and selectively read by the
model instead of being appended to every request.

A Hunch belongs to `symbiont-d`, not to the user profile. Conversation may
create or revise one, which can wake an exploration cycle. Exploration still
obeys the autonomy switch, daily token budget, and proactive-message limit.

Reflection is deliberately separate from PCP. PCP is the durable information
archive; Reflection is symbiont-d's time-aware working model of the
relationship. Raw observations and model interpretations live in separate
records. Episodes can overlap and form an acyclic parent graph. Hypotheses
retain evidence and alternatives, and a stable candidate reaches the
long-term profile only through the existing critical review path. A scheduled
follow-up only wakes exploration; the normal publication gate can still remain
silent.

## Requirements

- Rust 1.88 or newer.
- A recent `codex` CLI.
- An existing Codex login:

```bash
codex login status
```

## Run

For foreground development:

```bash
cargo run
```

Open [http://127.0.0.1:4317](http://127.0.0.1:4317).

Initialization is explicit. Autonomous exploration does not run until the
initial orientation is complete and autonomy is enabled in Settings.

### Connect Codex tasks

Install the local user skill once:

```bash
./scripts/install-codex-skill.sh
```

Then invoke it explicitly from any Codex task:

```text
$symbiont How does this decision relate to what I have been working through?
```

The current Codex model reads a bounded packet containing orientation, Current
Map, Open Loops, active Hunches, working hypotheses, and query-matched PCP
snippets. This does not start a second model run.

The reverse direction is disabled by default. Enable **读取 Codex 任务** under
Settings → 连接, then open **任务**. The list endpoint reads task metadata only.
Selecting a task performs a read-only `thread/read`; **带入对话** places a
bounded transcript in the composer for review and does not send or store it
automatically.

### Run as a macOS service

Install `symbiont-d` as a user LaunchAgent when it should remain available
outside the terminal that started it:

```bash
./scripts/service-install.sh
```

The installer builds the release binary, starts it at login, and configures
`launchd` to restart it after an unexpected exit. Re-run the installer after
changing the source.

```bash
./scripts/service-status.sh
./scripts/service-uninstall.sh
```

Service logs are written to `data/logs/`. Uninstalling the service preserves
all local data.

## Local Data

Runtime data stays under `data/` by default and is ignored by Git:

```text
data/context.sqlite3   PCP Pages and revisions
data/symbiont.sqlite3  usage and temporary trace details
data/reflection.sqlite3 interaction facts, Episodes, hypotheses, follow-ups
data/assets/           content-addressed image files
data/orientation.md    visible user orientation
data/profile.toml      initialization state
data/autonomy.toml     exploration policy and limits
data/reflection.toml   background interpretation policy and limits
data/compute.toml      model and reasoning-lane configuration
data/codex-bridge.toml explicit Codex task-access permission
```

The prototype does not silently import Codex task history. Conversation and
memory access remain scoped to data explicitly handled by this application.

## Development

```bash
cargo fmt --all -- --check
cargo test
```

The browser bundle is checked in. Rebuild it only after changing
`web/rich-text-source.js` or its dependencies:

```bash
npm install
npm run build:web
```

The read-only PCP CLI can inspect the local store:

```bash
cargo run --bin pcp -- doctor
cargo run --bin pcp -- scopes
cargo run --bin pcp -- search "query" text
cargo run --bin pcp -- read rev_...
```

## Source Layout

```text
crates/pcp-core/    protocol types and requests
crates/pcp-sqlite/ SQLite PCP implementation
src/bridge.rs       explicit Codex task and symbiont-context bridge
src/codex/          Codex app-server client, prompts, tools, traces
src/continuity.rs   conversation ingestion and PCP-facing context policy
src/conversation.rs in-flight message burst coordination
src/curiosity.rs    Hunch storage and Curiosity Map
src/exploration.rs  autonomous scheduling, budgets, and publication
src/reflection/     time-aware interaction analysis and projections
src/web.rs          local HTTP API
web/                embedded browser interface
integrations/       installable Codex skill source
```

## Known Limits

- `dynamicTools` in Codex app-server is experimental.
- Retrieval currently uses Summary, lexical, exact, temporal, and graph
  channels; there is no embedding index.
- Autonomous behavior and background memory maintenance still need long-running
  real-world evaluation.
- The current interface is a local web app. Service packaging currently covers
  macOS `launchd`; there is no native desktop shell yet.
