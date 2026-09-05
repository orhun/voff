# Delivery Roadmap

Each phase has an exit gate. Do not start later phases merely because placeholders are convenient.

## Phase 0 — Foundation

- Cargo workspace and core crate boundaries
- Core typed IDs, task model and validated state machine
- Canonical events and in-memory repositories
- Mock provider and deterministic scripted streams
- Minimal non-blocking Ratatui shell
- CI: format, clippy and tests
- Initial ADRs

Exit gate: core tests run without network/credentials; TUI starts and displays mock session/task state.

## Phase 1 — Single-agent vertical slice

- Versioned TOML config
- One real provider adapter
- Session creation and SQLite persistence
- Streaming conversation/events
- Cancellation and basic usage tracking
- Scoped read/search tools

Exit gate: user can configure one agent, run one task, cancel it and resume session history.

## Phase 2 — Safe code workflow

- Patch proposal artifact and base-digest conflict check
- Controlled test/process executor
- Approval UI and audit events
- Artifact inspector and diff view
- Secret redaction and terminal sanitization tests

Exit gate: agent can propose a patch and test it only through enforced permissions; denials and conflicts are visible and recoverable.

## Phase 3 — Multi-agent orchestration

- Formation editor/loader
- Structured delegation proposals
- Task graph scheduler and dependencies
- Depth, concurrency and budget policies
- Parent/child result handoff
- Integration/review task flow

Exit gate: a lead delegates two independent tasks to interchangeable agents, results are reviewed/integrated and graph state survives restart.

## Phase 4 — Provider breadth and resilience

- Second real provider adapter
- Shared provider contract suite
- Rate-limit, retry and circuit-break behavior
- Capability discovery/validation
- Cost estimates and provider health UI

Exit gate: the same agent profile can switch supported providers without engine changes, and provider faults do not freeze the UI.

## Phase 5 — Open-source beta

- Installation packages and release CI
- Onboarding wizard and example formations/prompts
- Import/export with redaction
- Contributor and provider-adapter documentation
- Threat-model review, fuzz/property tests and dependency policy
- Benchmarks for large task/event histories

Exit gate: a new contributor can build/test locally; a new user can complete documented solo and multi-agent tutorials.

## Post-MVP candidates

- Git worktree or container-isolated agent workers
- Local model runtimes
- Remote workers and SSH execution
- WASM/process-isolated plugin SDK
- Multi-user collaboration
- Headless JSON-RPC/daemon mode
- Reusable workflow templates and marketplace
- Web companion UI

These are candidates, not commitments. Promote them only after user evidence and a security design.

## Milestone tracking rule

For each completed item, link implementation, tests and documentation evidence in the project tracker. Do not use percentage-complete estimates without evidence.
