# ADR 0016: Versioned charter and team snapshots

## Status

Accepted

## Context

Formation configuration previously produced runtime `AgentProfile` values with
new UUIDs, but the stable project intent and team definition were not persisted
with the session. Editing TOML after a run therefore made it impossible to
inspect exactly which static definition created those runtime identities.

## Decision

- `ProjectCharter` and `TeamDefinition` are provider-neutral core types with an
  explicit schema version.
- `AgentDefinition` stores stable configured identity, role, provider/model,
  tools and delegation targets. `AgentProfile` remains the runtime instance
  with a generated `AgentId`.
- Every new session persists immutable, secret-free charter and team snapshots
  inside the existing serialized session record. Existing session rows remain
  readable because both fields default to absent.
- Workspace config may define a version 1 `[charter]`. When omitted, composition
  creates a conservative charter snapshot so even solo runs remain inspectable.
- Charter paths and privacy classes are validated before session start. Budget
  values are captured now; enforcement belongs to the following Phase 5B
  runtime-budget slice.

## Consequences

Diagnostic export automatically includes the exact charter and team definition
because it already serializes the session. No plaintext credentials enter the
snapshot. This slice does not yet replace the current agent repository with a
session-keyed instance repository, so resume still uses the existing runtime
composition path until the next Phase 5B slice.

