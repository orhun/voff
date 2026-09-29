# ADR 0026: Task-only composer and runtime role provider/model control

## Status

Accepted.

## Context

The Team Lead textarea previously multiplexed direct conversation, executable
tasks, and local slash commands. That distinction was easy to miss and made the
same input surface produce materially different orchestration behavior. Team
cards also exposed the local-CLI implementation sentinel `default`, but did not
show the provider text that best explains what a running role is doing. Changing
a role's provider or model after launch required leaving the workspace.

## Decision

Every non-empty Team Lead composer submission creates a persisted executable
root task and runs it through the normal Team Lead workflow. The textarea has no
slash-command parser; a leading slash is ordinary objective text. Quick Run
remains the explicit capital-`R` workspace action.

The Team view exposes `t` for membership and availability changes and `c` for
the highlighted role's provider and model. The setup-style popup can browse
provider routes already registered from workspace configuration, browse each
route's catalog models, or accept an exact model ID. Enter saves the active
`AgentProfile` through the repository, and the engine reads that profile when
it starts the role's next task. The runtime edit does not register new adapters,
rewrite project TOML, or survive a new run that rebuilds profiles from
configuration.

Team cards derive a bounded latest-response excerpt from persisted provider
`TextDelta` events. Restoring a session reconstructs the same projection. The
card keeps at most 1,024 response characters in memory and renders a short,
single-line excerpt. The local CLI model sentinel `default` is displayed as
`CLI automatic`; capability text such as `no delegation` is omitted. Modal
lists indicate selection with text styling rather than a selector glyph.

Task cancellation moves from `c` to `x` so role configuration has one stable,
mnemonic key without removing cancellation.

## Consequences

- All textarea submissions have one predictable persisted and inspectable task
  lifecycle.
- Existing slash-like text can still be assigned as work, but `/task` and
  `/tasks` no longer have special behavior.
- Users can see recent model output without opening the event viewer and can
  change a role's next-task provider and model without restarting the TUI.
- Runtime provider/model changes remain intentionally distinct from durable TOML
  configuration.
- Direct tool-free Team Lead conversation remains available in engine APIs for
  non-composer callers, but is no longer selected implicitly by textarea text.
