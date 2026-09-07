# ADR 0013: Dependent review and parent finalization

## Context

A lead can propose multiple delegated tasks in one provider response. The first
heterogeneous formation demo dispatched implementation and review as independent
children and completed the parent immediately after approvals. This allowed a
review to run without implementation evidence and could report success while a
child was still running or had failed.

## Decision

- A `review.request` child depends on the sibling children already delegated by
  the same parent.
- The scheduler supplies completed dependency summaries and artifact references
  to the dependent task as explicitly untrusted evidence.
- Approval completes non-delegation actions as before, but delegation leaves the
  parent running while children are active.
- The scheduler completes a running parent only after all direct children
  complete, aggregating their summaries and evidence references.
- A failed, cancelled, or blocked prerequisite fails queued dependants. Any such
  child also fails its running parent.

## Consequences

Implementation and review execute in deterministic waves, and failure cannot be
projected as parent success. Review ordering currently follows proposal order:
providers should emit implementation delegations before `review.request`.
Explicit provider-supplied dependency references remain a future protocol
extension.
