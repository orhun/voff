# ADR 0009: Bounded multi-agent orchestration

## Status

Accepted

## Decision

Formation configuration is validated before runtime and persisted as enabled agent profiles. Tasks reference trusted `AgentId` values; provider-supplied target names are resolved only against enabled profiles and the delegator's persisted allowlist. Normal assistant text never creates tasks.

Delegation creates parent/child tasks through the domain graph, which enforces depth, child-count and cycle constraints. Dependencies are durable. The scheduler promotes only dependency-satisfied queued tasks and runs at most the configured number per wave. Newly unblocked integration tasks run in later waves. A task result contains a bounded summary and artifact references and is available to its parent.

## Consequences

All agents currently share the configured provider and workspace safety boundary, while prompts and tools vary by task owner. Writes remain serialized by Phase 2 digest and approval controls. Provider breadth and per-provider resilience remain Phase 4 work.
