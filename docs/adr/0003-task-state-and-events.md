# ADR 0003: Task state and events

## Context
Task outcomes must remain trustworthy under cancellation and failure.
## Decision
Only domain methods validate task transitions. Repositories retain a current projection while every engine mutation emits a canonical event. Task graph dependencies are checked before insertion and delegation is bounded by depth and child count.
## Alternatives Considered
Public status mutation and deriving state only from an event log were rejected for invariant risk and unnecessary Phase 0 complexity.
## Consequences
Invalid and terminal transitions fail explicitly. Phase 1 can persist projection and event together transactionally.
## Status
Accepted

