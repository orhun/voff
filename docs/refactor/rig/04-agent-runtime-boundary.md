# Rig refactor: Phase 4 agent runtime boundary

Date: 2026-09-14

## Decision

Evaluate, but do not adopt, `rig-agent` 0.42.0 in the production runtime.
AgentMux continues to call the Rig-backed `ModelProvider` boundary for one model
turn at a time. AgentMux remains the sole authority for task/session state,
tool approval and execution, delegation, scheduling, budgets, audit and
persistence.

The exact `rig-agent = 0.42.0` package is included only as a
`default-features = false` development dependency of `agentmux-providers`.
Executable characterization tests pin the evaluated serialization and sans-I/O
protocol without increasing the production dependency graph or release binary.

## Evaluation matrix

| Rig 0.42 capability | Evidence | AgentMux disposition |
|---|---|---|
| `AgentRun` sans-I/O state machine | `next_step` yields explicit model/tool/done steps | Useful design, retained as test-only evaluation |
| Serializable suspended run | JSON round-trip preserves a pending model call | Do not persist; no second source of truth |
| `AgentRunner` streaming/blocking loop | Shared driver with hooks and turn budgets | Do not adopt; AgentMux already owns cancellation, retry and task lifecycle |
| Tool registry and execution | Runner dispatches registered tools itself | Reject for production because it would bypass or duplicate AgentMux approval/audit authority |
| Conversation memory | Runner can load/save a memory backend | Defer; AgentMux persists canonical session/task/events and has no product requirement for a second memory store |
| Hooks | Per-run observation and steering | Defer until a concrete provider-neutral observability gap exists |
| Agent identity/config | Runtime-local agent configuration | Keep AgentMux `AgentDefinition`, `AgentInstance` and `AgentProfile` authoritative |

## State ownership

There are three deliberately separate concepts:

1. **Domain state:** AgentMux `Session`, agent instances, tasks, dependencies,
   results, claims and budgets in SQLite.
2. **Audit log:** AgentMux canonical events recording proposals, approvals,
   denials, execution and state transitions.
3. **Provider turn state:** transient Rig request/stream values contained within
   `agentmux-providers` and normalized before crossing the boundary.

`AgentRun` JSON is not stored. Provider raw payloads, Rig tool registries and
conversation memory are not parallel production sources of truth. Consequently
a Rig breaking release cannot make an existing AgentMux session unreadable.

## Restart and compatibility evidence

- A fresh `AgentRun` round-trips in the Phase 4 characterization test.
- A suspended `AgentRun` round-trip retains the awaiting-model protocol
  position and refuses a duplicate model request.
- A manifest guard rejects `rig-agent` dependencies in core, provider port,
  engine, storage, TUI and CLI.
- A pre-runtime AgentMux session JSON fixture without charter/team fields opens
  through the current SQLite repository with both additions defaulted.
- Existing completed DAG reopen and CLI interrupted-task recovery tests remain
  the production restart contract.

## Consequences

This phase adds no production feature, configuration key, schema migration or
binary-size cost. It avoids two independent agent loops and prevents Rig's
direct tool runner from becoming an alternate authority. The trade-off is that
AgentMux continues to maintain its own intentionally product-specific turn,
approval and orchestration lifecycle while reusing Rig only for provider/model
infrastructure.

Reconsider production adoption only if a future single-agent requirement
cannot be met cleanly through `ModelProvider`. Any new proposal must first
provide an adapter that turns `CallTools` into approval-gated AgentMux commands,
uses versioned internal DTOs rather than persisting Rig JSON, and proves parity
for cancellation, restart, budgets and audit.

