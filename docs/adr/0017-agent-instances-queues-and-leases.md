# ADR 0017: Session agent instances, bounded queues and leases

## Status

Accepted

## Context

Static agent definitions were persisted in the Phase 5B team snapshot, but the
scheduler still addressed global runtime profiles directly. It had no durable
per-session execution identity, queue capacity or protection against two
concurrent tasks being started for the same single worker.

## Decision

- Each configured agent receives one session-scoped `AgentInstance` linking its
  stable definition ID to its generated runtime `AgentProfile` ID.
- `max_queue` is a validated agent-definition setting (default 8, maximum 1024).
  Delegation is rejected before persistence when the instance already owns that
  many non-terminal tasks.
- Before a task becomes `Running`, the engine atomically acquires a durable
  task lease for its instance. An unexpired lease prevents another task from
  starting on that instance. Scheduler waves select at most one ready task per
  instance.
- Normal provider completion, failure and cancellation release the lease.
  Unexpected exits leave it persisted; expiry permits safe later acquisition.
- Resume reuses stored instances and profiles instead of silently generating a
  new runtime identity. Diagnostic exports include instance and lease state.

## Consequences

SQLite schema migration 4 adds `agent_instances`. Legacy sessions without
instances retain the previous scheduler behavior. A later Phase 5B slice will
add direct assignment, path claims and explicit lease-reconciliation controls;
this lease intentionally covers agent execution ownership only.

