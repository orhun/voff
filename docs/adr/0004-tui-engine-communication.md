# ADR 0004: TUI and engine communication

## Context
Rendering must remain responsive during provider execution and cancellation.
## Decision
Use bounded Tokio channels for normalized UI actions and engine events. Run terminal input and providers outside rendering; the TUI owns only an `AppModel` projection and reducer. Streaming history is capped and control characters are sanitized.
## Alternatives Considered
Direct provider calls from widgets and unbounded broadcast channels were rejected due to coupling, freezes, and memory risk.
## Consequences
Backpressure and cancellation are explicit; render functions remain pure. High-volume coalescing can be added when measured.
## Status
Accepted
