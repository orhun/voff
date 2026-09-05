# ADR 0008: Phase 2 approval boundaries

## Status

Accepted

## Decision

AgentMux treats writes and processes as proposals. A file replacement carries the SHA-256 digest of the source it was prepared against and can only be applied after explicit approval; a changed digest produces a recoverable conflict. Process execution uses an executable plus argument vector, never a shell string. Only scoped Cargo verification commands are accepted, with workspace, timeout, environment and output bounds.

Approvals, decisions and artifacts are durable records. An approved process grant is constructed only from an approved `process.test` record and is consumed by execution. TUI approval is approve-once or deny; no session-wide implicit grant is introduced.

## Consequences

Model output alone never grants authority. Provider adapters and the UI must route proposals through the engine and persistent audit trail. Adding another command or write mode requires an explicit policy change and tests.
