# ADR 0034: Let coordinators answer simple questions directly

## Context

Every composer submission is persisted as a task. Forcing `task.delegate` on
every coordinator request also forced simple questions into specialist work;
a prompt alone could not override that tool choice.

## Decision

Use provider-neutral automatic tool choice for coordinators. Built-in and
runtime coordinator instructions classify requests and require direct answers
for simple questions, while retaining structured delegation for specialist
implementation. Configured solo coordinators also use automatic choice instead
of forcing a patch. Existing leaf implementation requests retain patch choice.
Existing configured profiles receive the runtime guidance without migration.

No permission, dispatch, approval, or retry limits change. Model classification
is guidance, not a deterministic guarantee of appropriate delegation. Tool
proposals still require runtime validation; prose cannot execute an action.

## Verification

Tests cover direct answers in the normal work path in team and solo mode,
without child tasks or approvals, and retained structured delegation and review
coverage. No additional classifier/provider round trip is introduced.
