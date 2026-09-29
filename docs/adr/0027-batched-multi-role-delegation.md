# ADR 0027: Batched multi-role delegation plans

## Status

Accepted.

## Context

The Team Lead previously received a tool whose arguments described one child
task and was instructed to call it repeatedly. Some providers emitted only the
first call even when an objective clearly spanned backend, frontend, testing,
and operations. The scheduler already supported concurrent independent
children, but it never received the missing proposals.

## Decision

- Define `task.delegate` as one bounded plan containing a `proposals` array.
- Include enabled target names, roles, and tool capabilities in the trusted
  coordination prompt so assignment is role-aware.
- Validate the entire plan against target membership, enabled state, depth,
  existing children, pending approvals, and the parent's child limit before
  persisting any part of it.
- Expand a valid plan into one approval and one eventual child task per item.
  Approval remains per action; the batch does not grant blanket authority.
- Keep accepting the earlier direct single-proposal arguments for compatibility
  with in-flight adapters and persisted tests.
- Continue using the existing bounded scheduler. Independent approved children
  may run concurrently; dependencies still serialize work when required.

## Alternatives

- Ask the provider to emit repeated parallel function calls. This produced the
  reported provider-dependent single-role behavior.
- Infer and create tasks from assistant prose. This would cross the structured
  action boundary and is prohibited.
- Automatically assign every configured role. This creates filler tasks and
  wastes budget when the objective needs only one specialty.

## Consequences

Broad objectives can produce a complete multi-role plan through one portable
structured call. Users still inspect and approve each delegation separately.
Simple objectives may legitimately contain one proposal, and all fan-out
remains bounded by runtime task limits.
