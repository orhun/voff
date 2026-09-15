# ADR 0005: SQLite persistence and recovery

## Context
Phase 1 requires resumable sessions without weakening task/event consistency.
## Decision
Use embedded SQLite with schema migrations, WAL mode, JSON-serialized versioned domain projections, canonical events, and usage rows. Task transition and its event share one transaction. A task left running after process termination is recovered as failed/interrupted, never completed.
## Alternatives Considered
Flat JSON files lack transactional transition/event updates. An ORM adds unnecessary Phase 1 surface area.
## Consequences
Session history survives restart and storage remains behind the repository port. Future domain representation changes require explicit migrations.
## Status
Accepted

The SQLite execution and migration mechanisms were refined by ADR 0016.
