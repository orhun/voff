# AgentMux product status

Status labels: `Implemented`, `Beta`, `Experimental`, `Planned`, `Blocked`,
and `Deprecated`.

## Verified baseline

| Area | Status | Evidence / limitation |
|---|---|---|
| Typed tasks, state machine and durable graph | Implemented | Core, storage and engine tests; SQLite resume |
| Ollama and OpenAI adapters | Beta | Shared contracts, streaming, tools, usage and cancellation |
| Heterogeneous agent routing | Beta | Named provider registry and bounded parallel waves |
| Approval-gated patch/test workflow | Beta | Digest-bound patching and allowlisted Cargo executor |
| New text-file creation | Beta | `file.create`, ADR 0014; existing parent directory required |
| Retry, circuit breaker and health | Implemented | Provider-neutral resilience contract tests |
| TUI task/event/approval inspection | Beta | Keyboard-driven and non-blocking; durable completion summary is shown in events |
| Redacted diagnostics and session selection | Implemented | `export`, `sessions`, `--resume`, `--session` |
| Release automation and dual license | Implemented | Tagged macOS/Linux archives, checksums, MIT OR Apache-2.0 |

## Delivery phases

### Phase 5A — Beta correctness (completed)

- `file.create`: Beta; approval, digest, absence, scope and atomic no-overwrite
  invariants are covered by unit and engine tests.
- Delegation duplicate and ancestor-loop guards: Implemented; revised criteria
  form bounded rework.
- Truncated tool-call detection and bounded repair: Implemented for OpenAI;
  partial tool calls have zero side effects and retry only before any event.
- Deterministic acceptance evidence gate / `NeedsWork`: Implemented for
  delegated criteria using persisted artifact evidence.
- Automatic reviewer node: Implemented when the formation has an enabled
  reviewer role.
- Durable, exportable completion summary: Implemented as a canonical event.

Exit gate: empty projects can be created safely; duplicate/partial actions have
zero side effects; required review and evidence block premature completion; a
single durable completion summary explains the result.

### Phase 5B — Team Runtime v2 (active)

- Versioned Project Charter and Team Definition: Beta; immutable, secret-free
  snapshots are persisted per new session (ADR 0016).
- Agent Definition versus runtime Agent Instance: Beta; stable configured
  definitions are separated from generated runtime `AgentProfile` identities.
- Session agent instances, bounded queues and expiring task leases: Beta;
  persisted by SQLite migration 4 and enforced by the scheduler (ADR 0017).
- Direct assignment and charter-bounded rework counters: Beta; provider-neutral
  engine APIs enforce enabled agents, queue capacity, state and attempt limits.
- Exclusive/shared path claims and deterministic write-conflict prevention:
  Beta; SQLite migration 5 persists claims and releases them on terminal/denied
  work (ADR 0018).
- Provider/privacy, token, cost and time budgets: Beta; dispatch and streaming
  checks, cancellation, durable denial events and explicit user rates are
  enforced (ADR 0019).

### Phase 6 — Team-first TUI and token UX (planned)

- Project/team setup wizard and role templates.
- TechLead-first summary with optional per-agent inspection.
- Session/task/agent/provider token and cost breakdown.
- Plain-language recovery actions for failures and blocked tasks.

### Phase 6.5 — Context efficiency and workflow as code (planned)

- Deterministic task compiler and optional optimizer preview.
- Checkpoint/reference-based context packets.
- Versioned reusable workflows and context-growth benchmarks.

### Phase 7 — Headless core and desktop client (planned)

- Local authenticated daemon/control API over the same engine.
- Thin Tauri client sharing persistence and permission semantics with the TUI.

### Phase 8 — Notifications and collaboration (planned)

- Redacted opt-in notification contract.
- Read-only notification adapters before remote-control capabilities.

## Key technical debt

- Multiple tool proposals in one model response can be ignored while an
  approval is pending; the protocol needs an explicit proposal batch policy.
- Large inline patch JSON can still hit provider limits; Phase 5A safely retries
  pre-output truncation, while content-addressed chunks belong to Phase 6.5.
- Criterion evidence currently references the task evidence set as a whole;
  finer-grained per-check attestations remain an enhancement.
- Cost remains an estimate based on user-configured rates; providers that report
  usage only at completion can be detected only after the billed response.
- A general directory-creation tool is intentionally absent, so `file.create`
  only targets an existing parent directory.
