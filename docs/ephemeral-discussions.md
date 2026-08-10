# Temporary discussions

Temporary discussions may read Symbiont's existing memory, but their new turns
do not enter Symbiont's memory or conversation pipelines unless the user
explicitly promotes them.

## Boundary

- The transcript is bounded, process-local RAM state.
- The normal chat write path is not reused: no PCP event, topic update,
  reflection input, signal promotion, continuation, or exploration trigger is
  created for a temporary turn.
- Inference is stateless. Each request will contain a bounded read-only memory
  seed, the bounded temporary transcript, and the current user turn.
- Runtime logs may retain payload-free lifecycle and usage metadata, but must
  not contain transcript text.
- "Temporary" means not retained by Symbiont's own memory chain. It does not
  claim cryptographic RAM erasure or override an upstream provider's data
  policy.

## Lifecycle

1. Start an in-memory session with explicit turn, character, idle-time, and
   concurrent-session bounds. The existing read-only Codex bridge supplies one
   bounded PCP context packet and recall result; the session cannot refresh or
   mutate that seed.
2. Append alternating user and assistant turns only after successful inference.
3. Hold the session when the user leaves temporary mode. Held sessions accept
   no new turns while the user decides what to do.
4. The user may resume, discard, or create a promotion draft.
5. A promotion draft has no write authority. The HTTP application boundary
   persists it only after an explicit user choice, then retires the RAM
   transcript. If the durable write fails, the transcript remains held.
6. Idle expiry or process exit discards the transcript.

## Promotion choices

- **Conclusion:** user-edited Markdown, with no claim that the discarded full
  transcript remains available as provenance.
- **Selected turns:** exact selected turns rendered with their roles.
- **Full transcript:** exact temporary transcript.

Discard remains the default. Promotion is the only bridge from temporary state
to Symbiont's durable memory chain. The pre-existing read-only memory seed is
never copied into a promotion draft.

## Current integration

Temporary discussions reuse the generic infer-runtime discovery, credentials,
and stateless Responses client. The composer exposes a distinct temporary mode,
restores a live process-local transcript after a page refresh, supports
interrupt, hold, resume, discard, conclusion promotion, and full-transcript
promotion. The first version is text-only and uses one complete non-streaming
Responses call per turn. Attachments, selective-turn controls, incremental
streaming, and speech-output policy remain intentionally separate follow-ups.
