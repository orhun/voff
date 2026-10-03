# ADR 0035: Contract-first, evidence-gated integrated work

## Context

Two children could generate unrelated scaffolds with incompatible APIs, receive
patch approval, and immediately complete their parent. Patch application proves
a file write, not that an application meets acceptance criteria.

## Decision

New tasks created by the TUI use durable `Task.work` metadata. Embedded engine
consumers opt in through `with_verified_workflow()`. Old tasks deserialize with
no metadata and retain their previous execution semantics; historical results
are not rewritten. Start a new root objective to use the new workflow on an old
workspace. No TOML or SQLite table migration is required.

For delegated implementation, `task.delegate` requires one shared contract,
implementation proposals and a separately assigned integration proposal. The
contract describes interfaces, file ownership, preservation requirements and
cross-component checks. The engine validates all targets, criteria, capabilities
and limits before dispatch. Integration counts toward the child limit and has
explicit persisted dependencies on all implementations. Role selection remains
configuration-driven. Direct answers remain tool-free and need no plan.

The plan is a single audited `task.plan` approval. In automatic mode the Lead's
own plan dispatches without self-review. In manual mode a human decides it.
Children cannot run while their parent is awaiting plan dispatch. An interrupted
partial dispatch fails closed rather than silently replaying children; it is
not a cross-record atomic transaction.

Workers can propose one patch or test per turn. Approved actions return the
task to Ready, with the updated workspace, artifact IDs and progress supplied
on the next turn. Patches are bound to their specific artifact/base digest,
including multiple revisions of the same file. The engine ignores its own
artifact correlation ID when checking repeated denied proposals.

`task.complete` supplies a summary and artifact-backed explanation for every
acceptance criterion. The engine rejects missing/foreign evidence and requires
a recorded successful test suite after the last approved workspace patch.
The Lead then reviews final acceptance, not just action safety. A denial follows
the existing feedback/revision loop. Parent completion requires all latest
implementation attempts and the integration task to complete. Prose alone
cannot complete delegated work or a root which has proposed actions.

Verified work is scheduled one task turn at a time in the shared checkout.
This deliberately trades concurrency for consistency until enforced file claims
or worktree isolation exist. Per-task turns are persisted and capped at 32,
including retries; the existing three-attempt repair limit also applies.
Cancellation and pending approvals continue to use the existing boundaries.

## Verification capabilities and limits

The executor accepts exact commands: Cargo test/check/clippy/fmt-check,
`python3 -m unittest discover`, `node --test`, `npm test`, and `npm run build`.
All are individually reviewed, run without a shell at the workspace root,
and use the existing timeout/output limits and a minimal environment (`CI=true`).
Only test suites qualify as completion evidence; build/check alone do not.
Package installation, arbitrary arguments, git and shell execution remain
unavailable. Projects need installed dependencies and a root-level test harness.

Allowlisted tests and npm scripts execute project code. This is not an OS or
network sandbox, and cannot prevent a malicious test from causing side effects.
Use manual review for untrusted workspaces. The executor command allowlist was
expanded explicitly; no agent gains a tool capability it did not already have.

The engine enforces evidence provenance, ordering and dependency gates, not the
semantic completeness of tests. A zero exit code does not prove coverage, and
the model reviewer can still make mistakes. Shared contract quality and test
adequacy remain review responsibilities. External filesystem edits are not
tracked by the event-based test freshness check. Review calls still incur cost;
32 turns is not a claim of cumulative token/cost budget enforcement.

## Tests

A deterministic provider drives two incompatible component outputs through
real workspace patches, an integration fix, real Cargo verification, final
acceptance and parent completion. Additional tests cover missing/failed/stale
and foreign evidence, denial, bounded retries, SQLite reopen, dependency gates,
cancelled parents, and exact command allowlists. No real provider or user
workspace is required.
