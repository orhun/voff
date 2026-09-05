# ADR 0001: Workspace boundaries

## Context
Phase 0 must establish inward-pointing architectural boundaries without implementing later-phase behavior.
## Decision
Keep all ten documented crates. Core, engine, provider port, adapters, storage, TUI, and CLI have working Phase 0 behavior. Config, tools, and executor expose deliberately small denial/contracts so future work has an explicit boundary without pretending Phase 1/2 functionality exists.
## Alternatives Considered
A single crate was simpler but would allow provider/UI types into domain code. Removing future adapter crates made later ownership less explicit.
## Consequences
Dependencies remain directional and compile independently; a small amount of manifest overhead is accepted.
## Status
Accepted

