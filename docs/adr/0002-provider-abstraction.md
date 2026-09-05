# ADR 0002: Provider abstraction

## Context
Roles and orchestration cannot depend on vendor SDK types.
## Decision
Define an async `ModelProvider` port with descriptors, capabilities, canonical requests, typed tool requests, bounded event sink, usage, normalized errors, and cancellation. Keep deterministic `MockProvider` in the adapters crate.
## Alternatives Considered
Vendor enums in core and free-form JSON callbacks were rejected because they couple the domain or weaken validation.
## Consequences
Adapters translate at the edge and contract tests require no credentials. Backpressure is explicit.
## Status
Accepted

