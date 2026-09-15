# ADR 0018: Persistent scope claims, assignment, and bounded rework

## Status

Accepted

## Decision

AgentMux persists session-scoped path claims. A claim is shared or exclusive;
overlapping paths conflict when either claim is exclusive and the claims belong
to different tasks. File creation and replacement acquire an exclusive claim
before publishing an artifact or requesting approval, and terminal or denied
work releases its claims.

Queued and `NeedsWork` tasks may be assigned directly to an enabled agent while
respecting that agent instance's queue capacity. Rework moves `NeedsWork` back
to `Ready`, clears the previous result, increments a persisted counter, and is
rejected after the Project Charter's `max_reworks_per_task` limit.

## Consequences

- Concurrent agents cannot silently propose writes to the same file or nested
  scope.
- Assignment and rework remain provider-neutral, durable, and auditable through
  canonical events.
- Claims are intentionally lexical workspace paths; filesystem containment and
  symlink validation remain the responsibility of the scoped workspace tool.
