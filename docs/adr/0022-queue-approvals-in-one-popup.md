# ADR 0022: Queue approvals in one popup

## Context

The engine can persist several approval requests in one scheduling wave. The
workspace previously rendered the first pending request in an application-drawn
side panel and only displayed the total count. Users could not inspect later
requests before deciding, and the panel competed with the team visualization on
wide terminals.

## Decision

Render pending approvals as one centered Ratatui modal. The modal presents
exactly one persisted request at a time, labels its position as `N of total`,
and uses Up/Down to move through the pending queue. Existing `y` and `n` actions
apply only to the visible approval ID. The popup shows the exact action in
yellow, plus the task, working directory, risk reason or structured delegation
details, and any task-local artifact preview. It also names the Team Lead
coordination route without implying that the lead replaces the user's approval
authority. The approve and deny shortcuts are rendered as a bottom-border title
so long or wrapped request content cannot displace them.

Approval state remains owned by the engine and repository. Popup selection is
session-local UI state and grants no authority. Requests are never combined into
one broader approval, and approving one cannot implicitly approve another.

## Consequences

Concurrent requests remain durable and independently auditable while the user
sees a single, unambiguous confirmation surface. Shared local layout helpers
keep the dialog centered and bounded on narrow terminals. Approval rendering,
action styling, persistent decisions, and queue navigation have TUI regression
coverage.
