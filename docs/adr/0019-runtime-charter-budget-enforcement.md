# ADR 0019: Runtime charter budget enforcement

## Status

Accepted

## Context

Persisting charter limits without checking them at dispatch and during provider
streams does not prevent resource overrun. Cost enforcement is impossible when
the user has not supplied rates, and model/provider identities must remain
configuration data rather than role-specific engine logic.

## Decision

Before dispatch, the engine totals durable session usage and checks token, cost
and elapsed-session limits. It caps the canonical request's output allowance to
the remaining token budget and installs a cancellation deadline bounded by both
the task and remaining session duration. Usage events are persisted before live
token/cost checks; an overrun cancels the provider, fails the task and emits a
canonical `BudgetExceeded` event.

Provider routes are restricted to the immutable Team Definition snapshot.
Declared `file.create` privacy may not exceed the charter privacy class. A
configured cost ceiling requires explicit input and output rates; AgentMux does
not embed volatile provider prices.

## Consequences

- Budget consumption and denials survive restart and appear in diagnostics/TUI.
- Providers that report usage only at completion may exceed a token/cost limit
  by their final response; AgentMux detects and fails that run but cannot undo
  provider billing.
- Privacy enforcement currently covers declared artifacts and provider route
  selection. Content-level classification remains out of scope for this beta.
