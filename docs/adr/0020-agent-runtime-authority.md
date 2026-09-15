# ADR 0020: Rig runtime under AgentMux authority

## Status

Accepted; incremental migration active.

## Context

Rig now provides the production OpenAI and Ollama normalization path. Using its
agent runtime can reduce custom turn-loop maintenance, but adopting direct Rig
tool execution, memory, or persistence would create a second authority beside
AgentMux's task graph, approvals, budgets, executor, and SQLite audit log.

## Decision

Introduce a provider-neutral `AgentRuntime` port. The engine owns every
state-changing decision and consumes runtime tool calls only as proposals. Rig
may drive a turn and suspend for a result, but AgentMux validates, authorizes,
executes, audits, and correlates that result before resuming the turn.

During migration, `ProviderAgentRuntime` wraps the existing provider contract.
This preserves behavior and gives a direct runtime-registry seam for a bounded
Rig `AgentRun` pilot. Persist versioned AgentMux checkpoint DTOs rather than
opaque Rig serialization.

## Consequences

- The engine no longer depends directly on provider turn execution.
- Existing provider configuration remains compatible.
- Tool approvals and exactly-once recovery remain mandatory migration gates.
- There is temporary compatibility-adapter code until runtime parity is proven.
- MCP, Rig memory, direct tool execution, and effect logging remain disabled.

## Rejected alternatives

- Replacing the engine with `AgentRunner`: loses AgentMux authority and recovery
  semantics.
- Persisting opaque `AgentRun`: couples stored sessions to Rig internals.
- Big-bang migration: makes rollback and behavioral comparison unsafe.
