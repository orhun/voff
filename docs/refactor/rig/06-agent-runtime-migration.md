# Rig Phase 6 — Agent runtime migration

Status: active; slice 1 complete.

## Target

Rig drives model/agent turns. AgentMux remains the sole authority for task DAG
state, delegation, budgets, cancellation, tool authorization/execution, audit,
and recovery. Provider or Rig tool calls are proposals, never authorization.

## Slice 1: stable runtime seam

- `AgentRuntime` is a provider-neutral, single-turn port.
- `ProviderAgentRuntime` preserves current production behavior while the
  migration proceeds.
- The engine dispatches through runtime instances rather than calling
  `ModelProvider::start` directly.
- Existing provider registries remain source-compatible and are wrapped at the
  composition boundary.
- A direct runtime registry is available for the future Rig single-agent pilot.
- A regression test proves tool requests are forwarded as events and are not
  executed by the compatibility runtime.

This slice intentionally makes no configuration or database schema change and
does not enable Rig's direct tool execution, memory, MCP, or effect log.

## Remaining slices

1. Implement a Rig `AgentRun` runtime adapter that suspends on tool calls.
2. Pilot it for an explicitly selected single-agent test route.
3. Persist only versioned AgentMux runtime checkpoints, including turn and tool
   correlation IDs; never serialize opaque Rig state as the database contract.
4. Resume after approved tool results and prove exactly-once side-effect
   behavior across interruption.
5. Move multi-agent task turns to the runtime port while keeping delegation in
   the AgentMux engine.
6. Remove the compatibility runtime only after parity, recovery, security,
   dependency, and live-provider gates pass.

## Exit gate

The migration is complete only when single- and multi-agent flows run through
the Rig runtime adapter, approvals cannot be bypassed, old sessions remain
readable, interruption cannot duplicate a tool effect, and the full offline and
opt-in live suites pass.
