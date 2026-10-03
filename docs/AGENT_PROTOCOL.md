# Agent Collaboration Protocol

## 1. Purpose

Define a provider-neutral, structured contract for tasks, messages, delegation, tools and results. Natural-language content is allowed inside fields, but state-changing intent must use typed envelopes.

## 2. Envelope

```json
{
  "protocol_version": "1.0",
  "message_id": "msg_...",
  "session_id": "ses_...",
  "task_id": "tsk_...",
  "sender_agent_id": "agt_...",
  "kind": "delegation_proposal",
  "created_at": "RFC3339 timestamp",
  "correlation_id": "cor_...",
  "payload": {}
}
```

The engine generates trusted IDs and timestamps when accepting provider output. Provider-supplied identity fields are untrusted suggestions.

## 3. Message kinds

- `task_brief`
- `progress_update`
- `clarification_request`
- `delegation_proposal`
- `delegation_decision`
- `tool_call_proposal`
- `tool_call_result`
- `artifact_published`
- `review_request`
- `review_result`
- `task_result`
- `task_failure`
- `task_blocked`

## 4. Delegation plan

New TUI work uses ADR 0035's contract-first extension:

```json
{
  "shared_contract": "Exact API routes, payloads, auth, ports, file ownership and end-to-end checks",
  "proposals": [
    {"target_agent":"Backend Developer","objective":"Implement the server against the shared contract","acceptance_criteria":["Server contract checks pass"]},
    {"target_agent":"Frontend Developer","objective":"Implement the client against the shared contract","acceptance_criteria":["Client checks pass"]}
  ],
  "integration": {"target_agent":"Frontend Developer","objective":"Connect and verify both components","acceptance_criteria":["End-to-end flows pass against the real backend"]}
}
```

Integration is a separate persisted child dependent on both implementations.
Approved patches/tests continue work instead of completing it. `task.complete`
requires a summary and `evidence` entries with zero-based `criterion`, persisted
`art_`-prefixed `artifact_ids`, and an `explanation`. A successful test after the
last workspace patch and final acceptance approval are required. Contract and
turn state survive restart. The legacy protocol below remains for stored tasks
and engine callers which have not enabled verified work.

The coordinating agent submits the complete bounded plan in one structured
`task.delegate` call. Each item remains a separate persisted task and a
separate approval action:

```json
{
  "proposals": [
    {
      "target_agent": "Backend Developer",
      "objective": "Implement the persistence repository",
      "acceptance_criteria": ["Repository contract tests pass"]
    },
    {
      "target_agent": "Frontend Developer",
      "objective": "Implement the independent user interface",
      "acceptance_criteria": ["The interface uses the documented API"]
    }
  ]
}
```

This single-call batch avoids provider-dependent reliance on parallel function
calling. The engine validates the whole batch before persisting any approval,
then fans it out into individual proposal records. A version-1 direct proposal
object remains accepted for adapter compatibility.

Validation:

- Target exists and is enabled.
- When a Team Lead is bound for the run, the sender is that Team Lead; a
  specialist cannot originate a root or delegation decision.
- Cross-specialist input is requested through the Team Lead. The lead decides
  whether to create a separate persisted task for the target specialist; a
  specialist never addresses or dispatches directly to that peer.
- Sender may propose or dispatch to target.
- Parent is not above depth/child/budget limits.
- The batch plus existing children and pending child approvals does not exceed
  the parent's child limit.
- Requested tools are a subset of target and session permissions.
- Context references belong to the session and are visible to the target.
- Adding the task/dependencies does not create a cycle.

## 5. Task result

```json
{
  "outcome": "completed",
  "summary": "Implemented repository and tests",
  "acceptance_evidence": [
    {
      "criterion_index": 0,
      "status": "satisfied",
      "evidence_refs": ["art_test_report"]
    }
  ],
  "artifact_refs": ["art_patch", "art_test_report"],
  "warnings": [],
  "follow_up_recommendations": []
}
```

The engine, not the model, decides whether all required evidence permits transition to completed.

## 6. Tool proposal

Every tool has a versioned JSON schema. A proposal includes tool name/version, typed arguments, reason, expected effect and idempotency hint. The engine performs policy evaluation and may return allowed, denied, approval-required or invalid.

Normal assistant text can never be interpreted as a tool call.

## 7. Context sharing

- Share task brief, selected messages, relevant artifacts, project rules and a bounded summary.
- Do not automatically share the entire history with every agent.
- Mark context with source and revision/digest.
- Never pass secrets unless the target tool/provider is explicitly authorized and the secret is required.
- Parent agents receive child summaries and artifacts; raw hidden provider reasoning is neither required nor expected.

## 8. Delegation policy

Formation-level policy fields:

- `mode`: `disabled | propose_only | auto_within_limits`
- `allowed_targets`
- `max_depth`
- `max_children_per_task`
- `max_parallel_children`
- `requires_approval_for_roles`
- `budget_inheritance`: `shared | allocated`
- `on_parent_cancel`: `cancel_children | detach | ask`

Default is `propose_only`, maximum depth 2 and explicit bounded budgets.

When an enabled coordinating agent has both allowed targets and the
`task.delegate` capability, the engine supplies each enabled target's role and
capabilities with automatic tool choice. The coordinator answers simple questions
directly without delegation, and uses a structured plan for specialist work.
Natural-language claims are not evidence of implementation. Leaf implementation
agents with `file.patch` are required to produce a structured patch proposal.
A configured solo coordinator also uses automatic tool choice so a question
does not force a workspace patch.

## 9. Review protocol

With `runtime.automatic_review = true`, routine proposal approvals return to the
configured coordinator through a separate restricted `workflow.decide` request.
The coordinator's own `task.delegate` and `review.request` assignments are an
exception: their validated decisions dispatch directly, retaining audit records
and runtime limits without requesting self-approval. Explicit manual mode
continues to require human decisions. Previously interrupted reviews still
fail closed rather than being silently authorized on resume.
The strict response is `{ "approve": true|false, "reason": "..." }`. Review has
no executable tools. Start/verdict events are persisted, and an affirmative
verdict still passes through the normal scoped dispatcher. Invalid verdicts
and review failures deny without execution. A valid Lead rejection of a patch or
test proposal returns feedback to its owner for a new attempt and fresh review
(ADR 0033); the denied action itself is not retried. Identical denied proposals,
human/runtime denials, cancellation and invalid reviews cannot enter this loop.
Repairable failures return to the coordinator for concrete corrective guidance,
bounded to three total attempts. Account/configuration issues notify the user;
output-token increases retain human consent. See ADR 0032.

A review request references immutable artifacts/diffs and acceptance criteria. Reviewer returns findings with severity (`blocking`, `major`, `minor`, `note`), location, rationale and suggested resolution. Only the integrator or user applies resolutions.

## 10. Concurrency and conflict rules

- Tasks may run concurrently when dependencies permit.
- File claims are advisory; base digests and patch checks are authoritative.
- Conflicting patches are not auto-merged in MVP.
- An integration task resolves conflicts and reruns affected checks.
- Results created from stale context must be marked stale, not silently applied.

## 11. Failure and retry

- Provider rate limits/timeouts may retry within policy using bounded exponential backoff and jitter.
- Invalid structured output may receive one repair request with the validation errors.
- Authentication, permission denial, budget exhaustion and user cancellation do not auto-retry.
- A tool-proposal error before approval emits failure evidence and transitions
  an already-running task to `Failed`; it cannot remain indefinitely active.
- Collect a bounded, complete provider response before publishing proposals.
  Invalid or incomplete responses cannot leave actionable partial approvals.
  Text and structured-output previews may be displayed during generation, but
  cannot authorize execution or prove completion. Every adapter requires a
  successful terminal record; truncated or filtered completions fail explicitly.
  Transient failures may retry within the attempt budget even after a preview:
  `ProviderRetrying` resets that preview, and failed-attempt proposals are
  discarded. Warnings are distinct from failures.
- Serialize scheduler waves and approval decisions. Independent child requests
  within a wave may run concurrently; a consumed approval cannot execute twice.
- Parent failure evidence identifies unsuccessful children by task ID/status.
- Scheduler waves operate only on tasks created or delegated in the active
  session; persisted queued tasks from older sessions are not resumed implicitly.
- Every attempt retains an attempt number and correlation ID.
- Retry never duplicates a non-idempotent side effect without reconciliation.

## 12. Versioning

Protocol versions use semantic versioning. Unknown major versions are rejected. Unknown optional fields in the same major version are ignored and preserved where practical. Persist the version used for every structured message.
