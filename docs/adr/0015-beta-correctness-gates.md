# ADR 0015: Phase 5A correctness gates

## Status

Accepted

## Context

Multi-agent runs could previously repeat equivalent delegations, complete a
delegated task using prose without persisted evidence, depend on the lead to
remember to request review, and report truncated provider tool arguments as a
generic invalid response. A completed root also lacked one durable summary
event suitable for the TUI and diagnostic export.

## Decision

- A normalized delegation signature consists of owner, objective and sorted
  acceptance criteria. Equivalent siblings are rejected, and repeating an
  ancestor owner/scope is rejected. Changed criteria form an explicit rework
  revision and remain allowed within the existing child/depth limits.
- Delegated tasks with acceptance criteria require persisted artifact evidence.
  Missing evidence produces structured `Unknown` criterion outcomes and moves
  the task to `NeedsWork`; it cannot finalize its parent.
- Finishing an evidence-producing non-review task automatically creates one
  reviewer child when an enabled reviewer role is present. The review depends
  on the producing task and inherits its evidence for criterion evaluation.
- Incomplete OpenAI responses and EOF-truncated tool argument JSON use the
  distinct `OutputTruncated` error. No partial tool request is emitted. The
  existing resilience adapter may retry it only when no event was emitted and
  only up to its configured attempt bound.
- Completing a root publishes one `CompletionSummary` canonical event with task,
  artifact, review and open-risk counts. Canonical persistence makes the same
  summary available after restart, in the TUI and in diagnostic exports.

## Consequences

Legacy serialized task results remain readable because criterion outcomes have
a serde default. `NeedsWork` is deliberately non-terminal and requires an
explicit transition back to `Ready` after new evidence or revised work. The
Phase 5A gate maps all criteria to the persisted evidence set; finer per-check
evidence production can evolve without weakening this minimum invariant.

