# ADR 0022: Queue approvals in one popup

## Context

The engine can persist several approval requests in one scheduling wave. The
workspace previously rendered the first pending request in an application-drawn
side panel and only displayed the total count. Users could not inspect later
requests before deciding, and the panel competed with the team visualization on
wide terminals.

## Decision

Render pending approvals as one centered modal using `tui-popup` 0.7.6 from the
maintained Ratatui `tui-widgets` repository. The modal presents exactly one
persisted request at a time, labels its position as `N of total`, and uses
Up/Down to move through the pending queue. Existing `y` and `n` actions apply
only to the visible approval ID. The popup continues to show the exact action,
task, working directory, risk reason or structured delegation details, and any
task-local artifact preview. It also names the Team Lead coordination route
without implying that the lead replaces the user's approval authority.

Approval state remains owned by the engine and repository. Popup selection is
session-local UI state and grants no authority. Requests are never combined into
one broader approval, and approving one cannot implicitly approve another.

## Consequences

Concurrent requests remain durable and independently auditable while the user
sees a single, unambiguous confirmation surface. The popup clears and centers
itself on narrow terminals through the third-party widget, while voff retains
control of the approval content and decision actions. `tui-popup` is pre-1.0, so
dependency upgrades require approval rendering and queue-navigation tests.
