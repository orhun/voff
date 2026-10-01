# ADR 0031: Output-limit consent and compact streaming

Status: Accepted

## Context

Code-producing responses can exhaust the former 4096-token default. Failing
the task immediately also fails its delegation tree, although the user may
prefer to authorize more generation. Raw long previews obscure current work,
and per-token SQLite writes add avoidable application-side overhead.

## Decision

Use a typed `OutputLimit` provider error only for an explicit token-limit finish
reason. Pause the task in `WaitingApproval` with a durable `provider.continue`
request. Approval makes the same task Ready with a persisted task-local doubled
allowance; denial makes it Cancelled. The scheduler accepts both newly queued
and already Ready work, without replaying consumed approvals. Conversation mode
remains tool-free after continuation. Parents wait while children await consent.

Continuation regenerates a complete response; it does not concatenate truncated
JSON or authorize partial patches. At most three consented increases are allowed,
up to 131072 tokens. Ordinary provider retries do not increase token allowances.
The default becomes 16384 in domain/config/setup; explicit existing settings
remain unchanged. No config/database schema migration is necessary.

Batch display-only generation fragments within 50 ms, flushing before type
changes and completion. Bound batch size and preserve stream ordering. Show
compact live tails, fold long details, and provide `v` to reveal stored content.
Active navigation uses a filled highlight without marker icons or dimmed alternatives.
Confirmation requests default to small top-right notices, retaining visible
yes/no controls. `d` opens a maximized, scrollable detail view; `d`/Esc minimizes
it without approving or denying the request. New decisions return to compact mode.

## Consequences

- More output may cost more and take longer. Model caps remain authoritative.
- Cancellation preserves history, not partial executable proposals.
- Unknown incomplete reasons, filters and malformed output remain failures.
- Waiting decisions survive database reopen. Failed historical tasks are not
  rewritten; users can re-run them explicitly.
- Reduced persistence overhead is not a claim of faster upstream inference.
- Tests cover typed classification, continue/deny, restart, duplicate decisions,
  stream coalescing, popup controls in narrow terminals and lossless expansion.
