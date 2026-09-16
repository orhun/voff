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

## 4. Delegation proposal

Required payload:

```json
{
  "target_agent_id": "agt_backend",
  "title": "Implement persistence repository",
  "objective": "Add the repository required by the parent task",
  "acceptance_criteria": [
    "Repository contract tests pass",
    "No provider-specific types enter core"
  ],
  "dependencies": [],
  "requested_tools": ["file.read", "file.patch", "process.test"],
  "limits": {
    "max_duration_seconds": 1800,
    "max_tokens": 50000,
    "max_cost_usd": 3.0
  },
  "context_refs": ["artifact_or_message_id"],
  "expected_output": "patch_and_test_report"
}
```

Validation:

- Target exists and is enabled.
- Sender may propose or dispatch to target.
- Parent is not above depth/child/budget limits.
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

## 9. Review protocol

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
- Every attempt retains an attempt number and correlation ID.
- Retry never duplicates a non-idempotent side effect without reconciliation.

## 12. Versioning

Protocol versions use semantic versioning. Unknown major versions are rejected. Unknown optional fields in the same major version are ignored and preserved where practical. Persist the version used for every structured message.
