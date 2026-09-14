# Input-only model roles

## Intent

Symbiont-d remains the one continuous conversational counterpart.  Other models may appear in the
timeline as **input-only roles**: they independently discover and describe external signals, but do
not receive ordinary conversation feedback, continue a thread, or answer the user.

This gives broad information acquisition a visible, attributable form without turning the chat into
a group conversation or treating every discovered item as memory.

## Runtime boundary

The continuous symbiont-d path remains deeply bound to Codex app-server: ordinary chat, PCP work,
strong review, and directed investigation all use that session and its tools.  Only the low-cost,
scheduled ambient sensing pass uses a separate Responses-compatible API adapter.  This deliberately
keeps broad external acquisition pluggable without weakening the capabilities or provenance of the
main counterpart.

Providers own a local endpoint, web-search tool type, and the *name* of an environment variable
containing an API key. Channels choose a provider plus their own role name, model, focus, and
cadence. This is deliberately not a fallback chain: a failed channel remains visibly failed with
its last error and last successful run, while other channels continue their separate remits. A
provider outage therefore never masquerades as another model's perspective. Secrets are never
written to disk or exposed to the browser. New user input cancels the in-flight adapter result
before it can enter review.

## Lifecycle

```text
ambient sensing role
  -> transient candidate pool
  -> exact identity suppression
  -> local model-assisted same-event classification (unrecoverable failure passes through)
  -> strong value review
     -> discard | input | deep
  -> signal timeline event
  -> user reply
  -> local transcript source packet + normal user/symbiont conversation
```

Candidates are a short-lived intake pool.  A new sensing pass replaces unpromoted candidates, and
candidates never write PCP.

A broadcast signal is retained locally with its actor snapshot, original text, sources, and attached
review annotations. Unreplied deliveries older than 24 hours are hidden from the chat during initial
loading or deliberate backward browsing. Visible or actively selected cards are preserved while the
user reads. The delivery clock, not the source document or event date, determines chat visibility.
Original inputs remain available in the date archive; the live projection limit does not delete them.
Local briefing topics are browsing labels, not PCP Topics or memory admission.

An explicit user reply attaches the exact source packet to the local transcript. It does not write a
PCP Page or stage a Context Inbox candidate. Actual live user references determine which old sources
remain in chat. Source packets use local signal identifiers only; there is no PCP revision mapping
or fallback read. Retracted replies no longer count as live references.

Formal PCP memory and candidate staging remain separate, selective model decisions based on the
content's future value. Merely receiving or replying to an external input does not establish that
value. Temporary operational activity is also separate from source storage and durable memory.
The retired automatic-deletion setting is no longer loaded or written; its old API returns HTTP 410.
Existing configuration files and archived evidence are left intact.

## Actor contract

Every signal stores an immutable actor snapshot:

- stable actor id;
- user-facing name and input-only label;
- model and effort that created it;
- deterministic avatar seed.

The snapshot belongs to the signal, rather than being reconstructed from the current compute
settings.  Changing a configured model therefore does not relabel past input.

`symbiont-d` remains the speaker for ordinary conversation and for any investigation it chooses to
run.  The reviewing model is provenance, not a second speaker: it may accept, reject, hold, or
escalate a candidate, but must not rewrite an accepted input into symbiont-d's voice.

## Review contract

Ambient sensing submits a bounded set of source-backed candidate drafts. Each draft includes a compact
natural-language proposed input, an actor snapshot, the underlying event date when known, and exact
source support.

Duplicate suppression is a separate bounded stage. Stable source identities are checked
deterministically; one local `text.deduplicate` request classifies residual candidate pairs as the
same paper, release, event, observation, recurring snapshot, or materially identical claim. Similar
topics are not duplicates, and a dashboard update with materially changed measurements remains new.
The parser accepts a complete bounded JSON object even when the local model omits a closing code
fence or adds harmless wrapper text. If the local stage is unavailable or the JSON object itself is
truncated, candidates pass through rather than blocking the pool or being silently lost.

The stronger value-review stage no longer compares history or emits duplicate targets. It reviews
bounded groups rather than one compound packet, and chooses one terminal disposition per candidate:

- `discard`: unsupported, unsafe, internally incoherent, or strong noise;
- `input`: retain the sensing role's wording and publish it as a signal;
- `deep`: hand the source packet to the continuous symbiont for directed work.

Valid decisions settle independently. A malformed or missing decision defers only its candidate,
not the whole batch. The review stage may qualify or reject a draft; substantive rewriting requires
an investigation and results in a symbiont-d message, not a falsely attributed signal.

## Timeline and reply contract

The API projects a typed timeline.  A `message` is a normal durable conversation entry; a `signal`
is an input-only local event.  The UI renders both as chat messages, but a signal has a distinct
avatar, speaker name, source footer, and a single meaningful interaction: reply.

Replying sends a signal reference, not a forged message quote.  The server resolves the reference
from the local signal store and gives the continuous symbiont the exact signal snapshot, sources,
and actor provenance.  A missing signal fails visibly instead of silently dropping the
context.

## Non-goals

- No multi-agent free-form conversation.
- No automatic PCP Page, candidate, profile, or Hunch write merely because an input was received or replied to.
- No automatic preference learning from ordinary response rate.
- No retroactive migration or reclassification of existing assistant messages.
