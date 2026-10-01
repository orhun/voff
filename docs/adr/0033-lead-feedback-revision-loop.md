# ADR 0033: Return rejected work for revision

## Context

The user wants a complete developer → Lead → feedback → developer → Lead loop.
A valid review rejecting an unsafe scaffold is feedback on the proposal, not
necessarily a reason to abandon the original task.

## Decision

In automatic mode, a valid coordinator rejection of a patch/test proposal
persists `RevisionRequested` after denying the original approval. The same
owner receives a new correlated attempt with the original parent, limits,
acceptance criteria and dependency guards. Its prompt includes the Lead's
feedback first (up to 16384 chars), separately from bounded previous rejected
proposal context (up to 49152 chars). Truncation is explicit; a large proposal
cannot displace feedback. The owner is asked to check and explain how each
finding was addressed, but prompt delivery is not proof of compliance.
A fresh proposal receives a fresh review;
there is no extra model call asking the Lead whether to send its own feedback.

The old attempt remains blocked and its action remains denied. Parents and
dependencies follow the successor attempt, so they wait for revision rather
than failing immediately. Existing return-to-Lead diagram edges show each new
review; activity and footer notices explicitly identify revise/resubmit and
stop conditions. Persisted revision requests resume without duplicating an
existing successor; interrupted active provider calls are not assumed safe to
replay. Older blocked tasks without a revision request require an explicit
re-run, which now includes saved review feedback and rejected proposal context.

## Boundaries

- The existing three-attempt budget includes revisions and error repairs.
- Human denials, runtime policy denials, cancellation, malformed verdicts,
  review transport failures and interrupted reviews do not start this loop.
- A model cannot set the internal validated-verdict flag via JSON.
- Exact denied argument JSON (ignoring object field order) cannot be submitted
  again through a retry and approved. Semantically equivalent but differently
  expressed proposals still require model review; this is not a semantic proof.
- Revisions cannot alter the owner's permissions, task criteria or token cap.
- Tasks which already delegated work are not automatically replayed.
- Reaching the bound or repeating a denied proposal emits `RevisionStopped`.
- A crash between denial and recording the revision request fails closed;
  explicit re-run retains the feedback. This is not a cross-record transaction.

## Verification

Tests cover rejected destructive scaffold → corrected patch → approval and
parent completion, feedback delivery, repeated-proposal prevention, exhaustion,
human denial and invalid/failed reviews. Existing cancellation and interrupted
review tests continue to protect fail-closed behavior.
