# ADR 0015: Use rig-core for provider transports

## Status

Accepted.

## Context

The Ollama and OpenAI adapters implemented HTTP payloads, streaming framing,
tool-call assembly, usage extraction, model listing, and provider error handling
directly. Maintaining those wire protocols duplicates mature provider work and
makes each upstream API change a voff concern.

voff still requires its own stable provider port because orchestration events,
cancellation, retry policy, approvals, and persisted data must not depend on a
pre-1.0 provider SDK.

## Decision

Use `rig-core` as the provider transport and normalization substrate. Concrete
adapters construct Rig clients and completion models, translate voff requests
into Rig's canonical request, drain Rig's normalized stream, and translate the
result back into voff events and errors.

Do not use `rig-agent`. voff continues to own the run loop, task graph,
delegation, tool authorization, execution, persistence, and recovery. Rig types
remain private to the adapter module and never enter `voff-core`, `voff-engine`,
or stored schemas.

## Alternatives

- Continue maintaining provider HTTP clients. This avoids a dependency but
  retains substantial protocol and conformance work.
- Replace voff orchestration with `rig-agent`. This would move approval,
  delegation, and durable task behavior across the security boundary and is
  incompatible with voff's product model.

## Consequences

OpenAI Responses and Ollama share Rig's normalized streaming and tool-call
handling. voff retains explicit cancellation and error classification and
treats a stream without Rig's terminal record as truncated. Rig upgrades are
isolated to one adapter module and require provider contract tests before being
accepted.
