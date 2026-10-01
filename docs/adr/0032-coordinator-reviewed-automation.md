# ADR 0032: Coordinator-reviewed routine work

## Context

The user explicitly requested unattended routine work: proposals and repairable
failures should return to the configured Team Lead, while account and runtime
configuration decisions remain human-owned. Missing parent directories also
made new nested-file proposals fail before review.

## Decision

The application defaults `runtime.automatic_review` to `true`; `false` restores
manual approvals. The reusable engine remains opt-in. This explicitly changes
the former per-action human-review default, including for existing configs
that omit the field.

Routine delegation, workspace patches and allowlisted verification commands
receive a separate provider-neutral request to the configured coordinator's
provider/model. Its only tool is the strict `workflow.decide` envelope
(`approve`, `reason`); it cannot execute tools. The engine persists review
start/verdict events and consumes the exact existing approval through its
serialized dispatcher. Malformed, cancelled, interrupted, or failed reviews
authorize nothing. Denial does not trigger automatic resubmission. Unknown
actions still fail dispatch. Model verdicts cannot extend tools, membership,
delegation limits, filesystem scope, or command allowlists.

### Follow-up: no self-review for assignments

In automatic mode, the configured Lead's own `task.delegate` and
`review.request` assignments now consume their exact grant directly through
the existing dispatcher. There is no additional model request or review UI
phase for those assignments: the Lead already made the decision. Runtime
target, authority and graph checks remain in place. Patches, verification and
repair reviews are unchanged, as is explicit manual mode. Already-started
interrupted reviews retain fail-closed recovery. Tests assert that delegation
creates no Lead-review event and saves one model request per assignment.

Recoverable file errors, invalid structured output, and failed verification
return to the coordinator with available evidence. Approved repair creates a
new attempt for the same owner, parent, limits and acceptance criteria, with
the Lead's correction instructions. Three attempts total are allowed. Tasks
that already dispatched children are not automatically replayed. Historical
failures stay failures; dependency and parent projections follow successors.
Authentication/authorization, availability and runtime limit problems notify
the user rather than changing configuration. Output-limit continuation remains
an explicit persisted human decision.

Review and workflow drivers are serialized. The asynchronous UI shows the
specialist-to-Lead return edge during review, review card status, verdicts and
reasons in activity, and compact application-issue notices. Routine approvals
do not interrupt with Y/N dialogs in automatic mode. Manual mode retains them.

Nested patch proposals inspect existing path components without mutation.
Missing directories are created only on approved application after checking
the original digest. Existing symlinks, including dangling links, are rejected
for writes; paths are revalidated after directory creation. Application state,
configuration and credential directories cannot be patched by agents.

## Limitations

- Review costs an additional model request per proposal/repair. It is not a
  deterministic code verifier or a substitute for tests.
- The executor still permits only its Cargo verification commands, not
  arbitrary shell execution, deployments or purchases.
- The executor is **not an OS sandbox**: build scripts/tests can execute code.
  Use manual review for untrusted projects. These checks do not guarantee
  network isolation of processes.
- Path/digest checks and serialized Voff writes do not protect against a
  malicious external process racing filesystem operations. Descriptor-relative,
  OS-enforced isolation remains future work; do not claim that guarantee.
- Interrupted reviews fail closed. Approval consumption is persisted before
  dispatch; uncertain side effects are not replayed.
- Repair plans can be wrong. On rejection or exhaustion the task remains
  failed/blocked; automation does not invent success.
- This does not replace the single-patch/test-per-response protocol with
  multi-file implementation turns or full acceptance-gate verification.

## Verification

Deterministic tests cover automatic review, denial, malformed verdicts,
delegation/repair/parent completion, duplicate drivers, bounded attempts,
human-owned token limits, and authentication notices. Filesystem tests cover
nested creation, denial and symlink attacks. Live provider tests remain opt-in.
