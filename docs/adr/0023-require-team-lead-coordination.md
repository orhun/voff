# ADR 0023: Require Team Lead coordination

## Context

Parallel specialists reduce latency, but peer-originated root tasks and nested
delegation can create overlapping ownership that is hard for a user to inspect.
The guided builder previously suggested a lead without requiring it, and the
runtime graph could display any configured delegation route without identifying
one integration authority. Users also needed a safe way to adjust an already
launched team without rewriting providers or active task ownership.

## Decision

Guided setup requires exactly one Team Lead in the first position. That role is
fixed during setup; later developers can select only specialist roles. Generated
formations grant delegation and review tools plus allowed targets only to the
lead, producing a lead-to-specialist star topology.
For a one-developer formation, the required lead receives scoped implementation
and test tools instead of delegation tools because there is no valid target.

At runtime, the application binds the configured entry agent to the engine with
`Engine::with_team_lead`. While that binding is present, creating root work and
originating or dispatching `task.delegate` or `review.request` from any other
agent fails with `TeamLeadRequired`. User approval remains a separate authority:
the Team Lead coordinates the route, while the user still approves or denies
each persisted side effect or delegation request.

Rataflow renders the bound Team Lead as a coordination hub, plus persisted
approval/task state. Each allowed specialist has one relationship connector to
the lead, representing assignments out and requests/results back. This makes a
Frontend-to-Backend relationship visible only through the two roles' shared
Lead hub; no peer-to-peer edge or peer delegation authority exists. This is a
projection of engine policy, not an editable authorization graph.

After launch, `S` opens a `tui-popup` team-adjustment modal. The user may enable
or disable an idle specialist for future assignments in the active runtime, or
press `n` to add a Backend, Frontend, Full-stack, Reviewer, Tester, or DevOps
specialist. A new specialist inherits the lead's provider, model, output limit,
and timeout, receives role-scoped tools, has zero delegation depth/children,
and is added only to the Team Lead's allowed targets.
The Team Lead is immutable, and a specialist cannot change availability while
it owns queued, ready, running, or approval-waiting work, or while a pending
delegation targets it. Accepted changes update the stored agent profile and
append `AgentAvailabilityChanged` to the session audit trail. Membership, role,
provider, and model customization remains a config-and-restart operation;
runtime-added preset membership applies to the active run and emits
`AgentAdded`.

The configured-run planner combines any required named workstreams with the
live formation. It preserves those assignments when their targets are enabled,
reassigns a required workstream to another enabled agent of the same role when
possible, and gives every remaining enabled Team Lead target one unique
role-specific support task. Disabled or unauthorized agents receive no task.
The lead's child limit grows with safely added runtime members so the persisted
approval queue can represent the full live team.

Generated support tasks use unique top-level artifact names rather than
assuming a `docs/` directory exists in an arbitrary target workspace. If tool
proposal validation or workspace preparation fails before an approval is
created, the engine records the failure and moves the task from `Running` to
`Failed`; it must never leak an indefinitely active task.

## Consequences

There is one deterministic coordination and integration point, specialists
cannot recursively dispatch competing work, and the graph visibly matches the
enforced route. Runtime availability can be narrowed or restored without
mutating active task ownership. This deliberately trades nested specialist
delegation for clearer ownership. The engine binding is optional so lower-level
tests and future non-team applications can use the provider-neutral engine
without hard-coding a role name.
