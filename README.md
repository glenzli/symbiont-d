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
- Continuous message bursts can interrupt the active Codex turn, then restart
  against the complete burst instead of spending the rest of a hidden draft.
- Optional short continuations use a second model turn only when the first turn
  explicitly reserved one; they are limited to one and canceled by user input.
- Latest-message recall, edit-as-resend, and recovery for interrupted sends.
- Direct connection to `codex app-server`; no separate model API pipeline.
- Host-side permission broker for Codex command, file, network, and additional
  permission requests, with one-turn or session decisions visible in chat.
- Controlled exact-page fetch path with redirect, size, content-type, public-IP,
  and per-domain permission boundaries.
- Explicit `$symbiont` skill for bringing a bounded live context packet into
  any Codex task without starting another model run.
- Opt-in Codex task browser: listing reads metadata only, and task content is
  fetched only after a user selects it.
- Explicit Codex task binding: after separately enabling code execution,
  symbiont-d can hand a concrete user-authorized repository change back into
  that task while preserving its working directory and context.
- Live model catalog and configurable compute lanes.
- Per-message compute constraints plus visible persistent topic rules. A matched
  rule starts directly in its configured minimum lane; the model can still
  request bounded escalation for semantic cases.
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
  |-- Permission Broker: interactive grants and background deny-by-default
  `-- local UI: conversation, settings, archive, usage, traces
```

The Codex thread provides short-term conversational flow. PCP is the durable
context boundary. Older information is searched and selectively read by the
model instead of being appended to every request.

A Hunch belongs to `symbiont-d`, not to the user profile. Opening a distinct
Hunch can wake an exploration cycle; routine revisions do not. When an
autonomous message materially surfaces a Hunch, PCP records that exact
relation. A user reply moves the Hunch through `feedback_pending`; Reflection
must revise, retire, or explicitly acknowledge it before exploration can use
the question again. Silence remains weak evidence. Exploration still obeys
the autonomy switch, daily token budget, and proactive-message limit.

Reflection is deliberately separate from PCP. PCP is the durable information
archive; Reflection is symbiont-d's time-aware working model of the
relationship. Raw observations and model interpretations live in separate
records. Episodes can overlap and form an acyclic parent graph. Hypotheses
retain evidence and alternatives, and a stable candidate reaches the
long-term profile only through the existing critical review path. A scheduled
follow-up only wakes exploration; the normal publication gate can still remain
silent.

Conversation is not treated as strict turn-taking. User messages sent while a
response is active join the same evolving burst and interrupt the current
Codex turn. The model may rarely reserve one 5–90 second continuation for a
distinct correction, association, or question. No reservation means no extra
model call; a reserved pass may remain silent and cannot reserve another pass.
New user input cancels it.

Longer delayed follow-ups remain separate. The conversational model or
Reflection can schedule one when time or new evidence matters. When due,
autonomous publication rechecks everything said since it was scheduled,
respects quiet hours, and may still remain silent. Proactive messages receive
recent timestamps and must bridge naturally when returning to an older or
adjacent topic.

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
snippets. It also receives up to four immutable image references, prioritizing
images attached to matched Pages before recent images; Codex inspects the chosen
local asset only when it matters. This does not start a second model run.

The reverse direction is disabled by default. Enable **读取 Codex 任务** under
Settings → 连接, then open **任务**. The list endpoint reads task metadata only.
Selecting a task performs a read-only `thread/read`; **带入对话** places a
bounded transcript in the composer for review and does not send or store it
automatically.

A selected task can also be explicitly bound to symbiont-d. Code execution is a
separate setting and remains disabled by default. Once both controls are active,
the interactive symbiont model may queue concrete implementation work the user
has requested. The operation resumes the original Codex task with a
workspace-write sandbox; network access, sandbox escapes, and other elevated
operations still pass through the Permission Broker. Progress is visible in the
task panel and the final Codex result returns as a normal symbiont message.
Image handoffs freeze exact PCP image Revision IDs before queuing, inject their
content-addressed files as Codex `localImage` inputs, and register images
generated by the bound task back into the symbiont conversation and PCP.
Discussion or speculative improvement ideas do not authorize a run.

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
data/compute-policies.toml
                       user-owned persistent topic compute rules
data/codex-bridge.toml explicit Codex task-access permission
data/task-runs.json    recent bound-task execution journal
```

The prototype does not silently import Codex task history. Conversation and
memory access remain scoped to data explicitly handled by this application.
Background runs never open an approval prompt. They can use the controlled
network path only when the domain was already granted for the current daemon
session; otherwise the request is declined and retained in the execution trace.

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
src/continuation.rs short conversational continuation lifecycle
src/continuity.rs   conversation ingestion and PCP-facing context policy
src/conversation.rs in-flight message burst coordination
src/curiosity.rs    Hunch storage and Curiosity Map
src/exploration.rs  autonomous scheduling, budgets, and publication
src/permission.rs   pending approval lifecycle and session grants
src/reflection/     time-aware interaction analysis and projections
src/task_execution.rs bound-task queue, lifecycle, and result publication
src/web_fetch.rs    permission-gated exact-page retrieval
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
