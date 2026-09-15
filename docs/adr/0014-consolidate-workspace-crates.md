# ADR 0014: Consolidate workspace crates

## Status

Accepted.

## Context

The initial workspace assigned separate crates to configuration, the terminal
UI, process execution, and concrete providers. Those boundaries helped establish
ports and adapters, but the implementations remained small, always changed with
their owning layer, and added manifests and dependency wiring without providing
independent reuse or release boundaries.

## Decision

Use six workspace crates:

- `voff-core` for provider-independent domain types and state machines;
- `voff-engine` for orchestration;
- `voff-provider` for the stable provider port and its adapters;
- `voff-storage` for repository ports and persistence adapters;
- `voff-tools` for workspace tools and controlled process execution;
- `voff` for configuration, composition, CLI commands, and the terminal UI.

Module boundaries continue to enforce ownership. Provider-specific types remain
inside provider adapters, process execution remains behind the executor API, and
Ratatui remains confined to the application crate.

## Alternatives

- Keep ten crates. This preserves maximum physical separation but retains
  disproportionate workspace and wiring overhead.
- Collapse the entire project into one crate. This would weaken the inward
  dependency boundaries required for isolated domain and engine testing.

## Consequences

The workspace has fewer manifests and cross-crate edges. Configuration and TUI
tests now run as application module tests, executor tests run with `voff-tools`,
and adapter tests run with `voff-provider`. The stable core, engine, provider,
storage, and tool boundaries remain independently testable.
