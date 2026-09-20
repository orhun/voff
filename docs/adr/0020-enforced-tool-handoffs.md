# ADR 0020: Enforced tool handoffs

## Status

Accepted for the current guided-team milestone.

## Context

A coordinating agent could ignore its delegation prompt and return an entire
implementation as assistant text. Leaf agents could do the same instead of
proposing a workspace change. OpenAI could then emit a structured tool call
followed by a terminal stream frame that Rig could not decode, causing the
engine to overwrite an already persisted approval with `Failed`. Restarted
sessions could also schedule queued tasks left by earlier sessions.

## Decision

The provider-neutral canonical request contains an explicit tool-choice policy.
An agent that has `task.delegate` and allowed targets must call
`task.delegate`. A leaf implementation agent with `file.patch` must call that
tool. Provider adapters translate canonical tool names into their native wire
names; role names and provider-specific request types do not enter the domain.

The engine remains the authority for structured proposals. Once it validates
and persists an approval request, an invalid terminal response frame is
recorded but cannot replace `WaitingApproval` with `Failed`. Authentication,
authorization, transport, cancellation, and other provider failures keep their
normal failure semantics.

Scheduler waves contain only root tasks created, or child tasks delegated, in
the active session. Historical queued tasks remain persisted for inspection but
are not executed by a later session.

At runtime, configured formation names are bound to the exact `AgentId` values
shown by the current TUI. Delegation resolves through this formation registry
before consulting persisted profiles. This prevents an older profile with the
same display name from owning a new child task that would be invisible on the
current graph.

Application-owned demos may provide a structured delegation plan when their
acceptance criteria intentionally require multiple independent workstreams.
The engine validates the complete plan against the current formation and task
limits, then persists one normal `task.delegate` approval per workstream. It
does not create any child until the user approves that handoff. This makes a
multi-agent demo deterministic without parsing prose, weakening approval, or
embedding role names in engine logic.

`file.patch` may create a new text file when its parent directory already
exists. The proposal records an absent-file base marker, and approval fails with
a conflict if another actor creates the path before application.

## Consequences

- Team leads cannot silently implement work that their formation requires them
  to delegate.
- Generated changes still require explicit user approval.
- Planned demo handoffs are visible and auditable before either child runs;
  denying them retains the same semantics as a model-proposed delegation.
- Empty workspaces can receive a first runnable file without granting general
  shell access.
- A malformed terminal stream event cannot discard a valid pending proposal,
  but it remains visible in the audit events and may omit usage data.
- Abandoned queued tasks require explicit recovery rather than being picked up
  by an unrelated later session.
