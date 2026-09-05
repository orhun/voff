# Delivery Roadmap

The binding roadmap is maintained in [`../promts/ROADMAP.md`](../promts/ROADMAP.md).

## Phase 0 evidence

- Workspace boundaries: `Cargo.toml`, `crates/*`, ADR 0001.
- Typed domain, state machine, task graph: `agentmux-core` tests.
- Canonical events and memory repositories: `agentmux-storage` tests.
- Deterministic mock streaming/cancellation/errors: `agentmux-providers` tests.
- Engine event recording and cancellation: `agentmux-engine` tests.
- Responsive TUI action reducer and bounded channels: `agentmux-tui` tests, ADR 0004.
- CI and ADRs: `.github/workflows/ci.yml`, `docs/adr/`.

## Phase 1 evidence

- Versioned TOML config and validation: `agentmux-config`, `.agentmux/config.example.toml`.
- Real provider: local-only Ollama adapter in `agentmux-providers`, ADR 0006.
- Session creation, SQLite migrations, atomic transitions/events and restart recovery: `agentmux-storage`, `agentmux-cli`, ADR 0005.
- Streaming, cancellation and usage: provider/engine contract tests and `usage_records`.
- Scoped read/search: `agentmux-tools` security tests, ADR 0007.
- Resumable single-agent composition: `agentmux-cli run`, configured Ratatui projection.
- Bounded workspace evidence supplied to the model: `ScopedWorkspace::context_bundle`, engine workspace context, traversal/size tests.

Phase 1 exit gate is complete: a configured Ollama agent can run and cancel a task, receives bounded workspace evidence, records streaming output/usage in SQLite, and restores the same session history after restart.

## Phase 2 evidence

- Patch proposal artifact, SHA-256 base digest, atomic apply and conflict denial: `agentmux-tools` security tests.
- Structured test executor without a shell, command allowlist, workspace boundary, timeout/output bounds and consumed approval grant: `agentmux-executor` tests.
- Approval/artifact domain events and SQLite migration: `agentmux-core`, `agentmux-storage` contract tests.
- Provider tool requests enter `WaitingApproval`; approval and denial are persisted and projected to the TUI: `agentmux-engine` integration test.
- Approval dialog (`y` once / `n` deny), restored pending approvals and task-local artifact diff preview: `agentmux-tui`.
- Terminal control sanitization and basic credential redaction: `agentmux-tui` tests.
- Scoped Phase 2 capabilities are explicit in configuration: `file.patch` and `process.test`; arbitrary shell remains rejected.

Phase 2 exit gate is complete: Qwen3/Ollama structured tool proposals enter durable approval, the TUI shows a digest-bound diff, approval dispatches only through scoped patch/test boundaries, and denial/conflict/failure remain visible and recoverable. Engine and executor acceptance tests cover approved patch application and approved Cargo verification.

## Phase 3 evidence

- Backward-compatible formation loader, entry-agent/target/tool validation and example: `agentmux-config`, `.agentmux/formation.example.toml`.
- CLI composition persists all formation agents and selects the configured entry agent; each task uses its owner profile's prompt and tools.
- Structured `task.delegate`/`review.request` proposals enforce enabled and allowed targets plus inherited depth/child limits.
- Parent/child graph, cycle-safe dependencies, bounded wave scheduler, child result/evidence handoff and integration ordering: `agentmux-engine` acceptance tests.
- Durable tasks, results and dependencies with SQLite schema migration 3 and reopen test: `agentmux-storage`.
- TUI displays formation agents, indented task trees and delegation/dependency/result events; approved delegation starts the bounded scheduler.

Phase 3 exit gate is complete: a lead can dispatch bounded structured work to interchangeable enabled agents, two independent workers run in one bounded wave, a dependent integration/review task runs afterward, results are handed back to the parent, and the graph survives SQLite reopen.

## Phase 4 evidence

- Provider-neutral bounded retry and circuit breaker: `agentmux-provider::ResilientProvider`.
- Retry classification permits only explicitly retryable rate-limit, timeout and internal failures.
- A provider run is retried only before the first streamed event, preventing duplicated text and tool side effects.
- Health snapshots expose circuit state, consecutive failures, retry count and the last error.
- Contract tests cover recovery, no retry after streaming and open-circuit fail-fast behavior.
- CLI-created mock, Ollama and OpenAI providers are wrapped by the same resilience boundary.
- The OpenAI Responses API adapter maps canonical messages, SSE text, strict function calls and usage without engine changes; credentials remain environment-only (ADR 0011).
- Adapter tests share the canonical streaming/cancellation/error vocabulary and include OpenAI text, usage and function-call mapping.
- `doctor` reports discovered provider capabilities and connectivity or actionable credential/endpoint failures.
- User-supplied per-million token rates drive explicitly labelled TUI cost estimates; absent rates remain visibly unconfigured.
- The TUI projects live circuit state, retry/failure counters and the last provider error without blocking input handling.

Phase 4 exit gate is complete: the same configured agent runtime can select Ollama or OpenAI without engine changes, provider faults are bounded by retry/circuit policies, health remains visible, and no fault path blocks the TUI event loop.
