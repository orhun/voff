# ADR 0021: Adopt rataflow for team visualization

## Context

ADR 0017 introduced the interactive developer graph with application-owned card
layout, hit testing, drag offsets, orthogonal edge routing, port drawing, and
animation. That implementation preserved the required domain boundary, but it
duplicated generic graph-widget behavior inside `tui.rs` and made viewport
features costly to add. The workspace already uses Ratatui 0.30, which is the
version supported by `rataflow` 0.1.

## Decision

Use `rataflow` 0.1 in the `voff` application crate for the runtime team graph.
`rataflow` owns presentation-only node positions, step-edge routing, selection,
mouse dragging, panning, zooming, fit-view, and animation timing. Voff provides
a custom `NodeContent` implementation for the existing status-rich developer
cards and derives every edge from structured state:

- one faint Lead/specialist relationship route comes from each
  `AgentProfile::allowed_targets` entry;
- approval routes come only from persisted delegation approvals;
- active and terminal routes come only from persisted parent/child tasks;
- connector edges carry no text; task objectives and textual status stay on
  owner cards while marker, color, stroke, and animation project route state.

When a runtime Team Lead is bound, voff renders exactly one connector per
allowed specialist. The connector represents the mediated relationship in both
directions: the lead assigns work and the specialist returns requests/results
through that lead. Running work animates the connector, and no
specialist-to-specialist edge is created. Disabled specialists remain visible
and their connector becomes subdued, matching the engine's dispatch check.

Mouse-selecting a role stores only a presentation-local highlight and recolors
every incident Lead relationship connector yellow. It does not select an edge,
change policy, or create a direct specialist relationship.

Nodes and edges are not connectable, deletable, resizable, or reconnectable
through the visualization. Graph interactions therefore cannot alter formation
policy, task ownership, or engine state. All provider-neutral domain and engine
types remain independent of Ratatui and `rataflow`.

Use the published crate release rather than an unpinned Git dependency. Keep
the adapter in `voff::tui::team_flow` so replacing the widget does not affect
the orchestration core.

## Consequences

The custom cell-routing, hitbox, and drag-offset implementation is removed,
along with the separate spinner dependency. The team graph gains pan, zoom,
fit-view, built-in edge crossing behavior, and semantic interaction events.
Live handoffs use animated double-line routes, explicit source/target markers,
and pulsing worker borders while retaining non-color `LIVE` text on the card.
Card positions and viewport state remain session-local presentation state and
are not persisted as domain data. Upgrading `rataflow` requires TUI rendering
and interaction tests because its pre-1.0 API may change.
