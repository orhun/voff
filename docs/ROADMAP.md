# Delivery Roadmap

This document records delivered milestone evidence and the remaining beta work.
Future scope is promoted only after its security and product requirements are
documented.

## Phase 0 evidence

- Workspace boundaries: `Cargo.toml`, `crates/*`, ADR 0001.
- Typed domain, state machine, task graph: `voff-core` tests.
- Canonical events and memory repositories: `voff-storage` tests.
- Deterministic mock streaming/cancellation/errors: `voff-providers` tests.
- Engine event recording and cancellation: `voff-engine` tests.
- Responsive TUI action reducer and bounded channels: `voff-tui` tests, ADR 0004.
- CI and ADRs: `.github/workflows/ci.yml`, `docs/adr/`.

## Phase 1 evidence

- Versioned TOML config and validation: `voff-config`, `.voff/config.example.toml`.
- Real provider: local-only Ollama adapter in `voff-providers`, ADR 0006.
- Session creation, SQLite migrations, atomic transitions/events and restart recovery: `voff-storage`, `voff-cli`, ADR 0005.
- Streaming, cancellation and usage: provider/engine contract tests and `usage_records`.
- Scoped read/search: `voff-tools` security tests, ADR 0007.
- Resumable single-agent composition: `voff-cli run`, configured Ratatui projection.
- Bounded workspace evidence supplied to the model: `ScopedWorkspace::context_bundle`, engine workspace context, traversal/size tests.

Phase 1 exit gate is complete: a configured Ollama agent can run and cancel a task, receives bounded workspace evidence, records streaming output/usage in SQLite, and restores the same session history after restart.

## Phase 2 evidence

- Patch proposal artifact, SHA-256 base digest, atomic apply and conflict denial: `voff-tools` security tests.
- Structured test executor without a shell, command allowlist, workspace boundary, timeout/output bounds and consumed approval grant: `voff-executor` tests.
- Approval/artifact domain events and SQLite migration: `voff-core`, `voff-storage` contract tests.
- Provider tool requests enter `WaitingApproval`; approval and denial are persisted and projected to the TUI: `voff-engine` integration test.
- Approval dialog (`y` once / `n` deny), restored pending approvals and task-local artifact diff preview: `voff-tui`.
- Terminal control sanitization and basic credential redaction: `voff-tui` tests.
- Scoped Phase 2 capabilities are explicit in configuration: `file.patch` and `process.test`; arbitrary shell remains rejected.

Phase 2 exit gate is complete: Qwen3/Ollama structured tool proposals enter durable approval, the TUI shows a digest-bound diff, approval dispatches only through scoped patch/test boundaries, and denial/conflict/failure remain visible and recoverable. Engine and executor acceptance tests cover approved patch application and approved Cargo verification.

## Phase 3 evidence

- Backward-compatible formation loader, entry-agent/target/tool validation and example: `voff-config`, `.voff/formation.example.toml`.
- CLI composition persists all formation agents and selects the configured entry agent; each task uses its owner profile's prompt and tools.
- Structured `task.delegate`/`review.request` proposals enforce enabled and allowed targets plus inherited depth/child limits.
- Parent/child graph, cycle-safe dependencies, bounded wave scheduler, child result/evidence handoff and integration ordering: `voff-engine` acceptance tests.
- Durable tasks, results and dependencies with SQLite schema migration 3 and reopen test: `voff-storage`.
- TUI displays formation agents, indented task trees and delegation/dependency/result events; approved delegation starts the bounded scheduler.

Phase 3 exit gate is complete: a lead can dispatch bounded structured work to interchangeable enabled agents, two independent workers run in one bounded wave, a dependent integration/review task runs afterward, results are handed back to the parent, and the graph survives SQLite reopen.

## Phase 4 evidence

- Provider-neutral bounded retry and circuit breaker: `voff-provider::ResilientProvider`.
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

## Phase 4.5 evidence

- Backward-compatible named provider registry and agent route/model validation: `voff-config`, `.voff/heterogeneous.example.toml`.
- Task-owner routing without provider-specific engine branches: `Engine::with_provider_registry`, ADR 0012.
- CLI builds one independently resilient adapter per named route and persists route/model identity in every agent profile.
- The TUI lists each formation agent with its selected provider and model.
- Acceptance tests run two agent-owned tasks through distinct routes in one bounded wave and prove that one provider failure does not cancel its sibling route.

Phase 4.5 exit gate is complete: a single formation can route concurrent agents to independently configured Ollama and OpenAI adapters while retaining the same task graph, permission boundary and persistence model.

## Phase 4.5 orchestration hardening

- Approved implementation and review proposals form dependency-ordered waves;
  review receives completed prerequisite summaries and evidence references.
- Delegating parents remain active until all direct children finish and aggregate
  their normalized results only on success.
- Failed, blocked, or cancelled prerequisites fail queued dependants and prevent
  successful parent completion.
- Engine acceptance tests cover heterogeneous implementation/review handoff and
  child-failure propagation (ADR 0013).

## Phase 5 progress

- Root README now provides local and heterogeneous-team onboarding, secure
  credential setup, TUI controls, and contributor quality commands.
- `voff init` interactively creates a validated solo Ollama or OpenAI
  configuration, supports non-interactive arguments, keeps credentials
  environment-only, and refuses accidental overwrite.
- CI enforces the checked-in `cargo-deny` dependency policy.
- Tag-triggered release CI builds locked Linux x86-64 and macOS Intel/Apple
  Silicon archives, generates SHA-256 checksums, and publishes them only for
  version tags. The maintainer procedure is documented in `docs/RELEASING.md`.
- `voff run` creates a clean session by default; `--resume` explicitly
  reopens the newest session. Initial TUI projections and interruption recovery
  are scoped to the selected session, preventing unrelated historical tasks
  from being mixed into a new run.
- `voff export` writes a session-scoped diagnostic JSON bundle, redacts the
  configured credential value and common secret forms, and refuses accidental
  overwrite.
- Root security and conduct policies define private reporting, supported beta
  revisions, coordinated disclosure, community expectations, confidential
  enforcement, and appeals without inventing an unverified contact address.
- Project and contribution metadata use the standard `MIT OR Apache-2.0`
  dual-license expression, with both complete license texts included.
- Provider contributors have a boundary/security guide and reusable contract
  test support for canonical requests, bounded event collection, descriptors,
  completion ordering, and usage consistency.
- Provider contracts also validate canonical failures, retryability,
  cancellation, and structured tool-call JSON.
- Generated task graph/state invariants, malformed provider/redaction corpora,
  and a reproducible 10,000-event SQLite benchmark cover Phase 5 robustness.
- Beta metadata is `0.1.0-beta.1`; changelog, release checklist, and release-CI
  archive self-verification are checked in.
- Onboarding selects solo or lead/implementer/reviewer templates, while
  `sessions` and `run --session` make stored runs discoverable and selectable.
