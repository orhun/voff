# Delivery Roadmap

This document records delivered milestone evidence and the remaining beta work.
Future scope is promoted only after its security and product requirements are
documented.

## Phase 0 evidence

- Workspace boundaries: `Cargo.toml`, `crates/*`, ADR 0001.
- Typed domain, state machine, task graph: `voff-core` tests.
- Canonical events and memory repositories: `voff-storage` tests.
- Deterministic mock streaming/cancellation/errors: `voff-provider` adapter tests.
- Engine event recording and cancellation: `voff-engine` tests.
- Responsive TUI action reducer and bounded channels: `voff` TUI tests, ADR 0004.
- CI and ADRs: `.github/workflows/ci.yml`, `docs/adr/`.

## Phase 1 evidence

- Versioned TOML config and validation: `voff` config module, `.voff/config.example.toml`.
- Real provider: local-only Ollama adapter in `voff-provider`, ADR 0006.
- Session creation, SQLite migrations, atomic transitions/events and restart recovery: `voff-storage`, `voff`, ADR 0005.
- Streaming, cancellation and usage: provider/engine contract tests and `usage_records`.
- Scoped read/search: `voff-tools` security tests, ADR 0007.
- Resumable single-agent composition: `voff run`, configured Ratatui projection.
- Bounded workspace evidence supplied to the model: `ScopedWorkspace::context_bundle`, engine workspace context, traversal/size tests.

Phase 1 exit gate is complete: a configured Ollama agent can run and cancel a task, receives bounded workspace evidence, records streaming output/usage in SQLite, and restores the same session history after restart.

## Phase 2 evidence

- Patch proposal artifact, SHA-256 base digest, atomic apply and conflict denial: `voff-tools` security tests.
- Structured test executor without a shell, command allowlist, workspace boundary, timeout/output bounds and consumed approval grant: `voff-tools` executor tests.
- Approval/artifact domain events and SQLite migration: `voff-core`, `voff-storage` contract tests.
- Provider tool requests enter `WaitingApproval`; approval and denial are persisted and projected to the TUI: `voff-engine` integration test.
- Queued centered approval dialog (`Up`/`Down` inspect, `y` once / `n` deny),
  persistent border controls, restored pending approvals, and task-local artifact
  diff preview: `voff` TUI module.
- Terminal control sanitization and basic credential redaction: `voff` TUI tests.
- Scoped Phase 2 capabilities are explicit in configuration: `file.patch` and `process.test`; arbitrary shell remains rejected.

Phase 2 exit gate is complete: Qwen3/Ollama structured tool proposals enter durable approval, the TUI shows a digest-bound diff, approval dispatches only through scoped patch/test boundaries, and denial/conflict/failure remain visible and recoverable. Engine and executor acceptance tests cover approved patch application and approved Cargo verification.

## Phase 3 evidence

- Backward-compatible formation loader, entry-agent/target/tool validation and example: `voff` config module, `.voff/formation.example.toml`.
- CLI composition persists all formation agents and selects the configured entry agent; each task uses its owner profile's prompt and tools.
- Structured `task.delegate`/`review.request` proposals enforce enabled and allowed targets plus inherited depth/child limits.
- Parent/child graph, cycle-safe dependencies, bounded wave scheduler, child result/evidence handoff and integration ordering: `voff-engine` acceptance tests.
- Durable tasks, results, dependencies and local credentials with SQLite schema migration 4 and reopen tests: `voff-storage`.
- TUI displays formation agents, indented task trees and delegation/dependency/result events; approved delegation starts the bounded scheduler.

Phase 3 exit gate is complete: a lead can dispatch bounded structured work to interchangeable enabled agents, two independent workers run in one bounded wave, a dependent integration/review task runs afterward, results are handed back to the parent, and the graph survives SQLite reopen.

## Phase 4 evidence

- Provider-neutral bounded retry and circuit breaker: `voff-provider::ResilientProvider`.
- Retry classification permits only explicitly retryable rate-limit, timeout and internal failures.
- A provider run is retried only before the first streamed event, preventing duplicated text and tool side effects.
- Health snapshots expose circuit state, consecutive failures, retry count and the last error.
- Contract tests cover recovery, no retry after streaming and open-circuit fail-fast behavior.
- CLI-created mock, Ollama and OpenAI providers are wrapped by the same resilience boundary.
- The OpenAI Responses API adapter maps canonical messages, SSE text, strict function calls and usage without engine changes; configuration retains only a credential lookup name (ADRs 0011 and 0018).
- Adapter tests share the canonical streaming/cancellation/error vocabulary and include OpenAI text, usage and function-call mapping.
- `doctor` reports discovered provider capabilities and connectivity or actionable credential/endpoint failures.
- User-supplied per-million token rates drive explicitly labelled TUI cost estimates; absent rates remain visibly unconfigured.
- The TUI projects live circuit state, retry/failure counters and the last provider error without blocking input handling.

Phase 4 exit gate is complete: the same configured agent runtime can select Ollama or OpenAI without engine changes, provider faults are bounded by retry/circuit policies, health remains visible, and no fault path blocks the TUI event loop.

## Phase 4.5 evidence

- Backward-compatible named provider registry and agent route/model validation: `voff` config module, `.voff/heterogeneous.example.toml`.
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

- First interactive launch now builds an arbitrary 1–32 developer formation,
  using one page for team size/token defaults and suggested names and roles to
  minimize input. Exactly one Team Lead is required and generated teams grant
  delegation authority only to that entry agent. Provider and model selection exists only on each developer.
  OpenAI tokens are offered as an optional masked setup field and can be added
  after launch with `A`; missing credentials never block the workspace. Values
  are stored in the workspace-local SQLite database rather than project
  configuration (temporary plaintext-at-rest tradeoff, ADR 0019). The model
  catalog also supplies provider-qualified per-agent cost estimates, with
  explicit manual-rate overrides and honest unknown/partial states.
  Existing configs and scripted `voff init` usage remain compatible.
- A `ratatui-textarea` composer sends plain text to the entry Team Lead as a
  persisted, tool-free conversation task; `/task <objective>` submits
  executable work with delegation enabled, and `/tasks` focuses the current
  task list without provider I/O. Enter focuses the composer; once focused, its
  hint switches to the available conversation commands. `? help` sits beside
  the working directory on the bottom-right border. Background submission
  failures return to the composer visibly.
- The workspace presents developers as status-rich cards connected by their
  explicit Team Lead-mediated delegation relationships. Each specialist has
  one connector to the lead for assignments, requests, and results, so a
  cross-role relationship is shown only through the shared Lead hub.
  The engine rejects specialist-originated root or delegation decisions and
  never renders direct peer edges. Static permission routes stay faint;
  persisted cross-owner child tasks become directional routes with a double-line
  animated live state. Objectives and status remain on owner cards; connector
  lines carry no text. Each card renders one icon-labeled role identity inside
  its border rather than repeating it as a title. `rataflow` owns route rendering, node
  dragging, selection, pan, zoom, and fit-view while voff supplies the
  provider-neutral card and handoff projections. Cards support mouse selection
  and dragging while all workflow controls remain keyboard-accessible.
- `S` opens a centered runtime team editor that enables or disables idle
  specialists or adds safe role presets inheriting the lead's provider/model.
  It keeps the Team Lead immutable, refuses availability changes around active
  or approval-pending work, strips peer-delegation authority, and persists team
  audit events. Rataflow immediately projects the updated lead routing.
- The Rataflow team canvas is the default fullscreen center view. `1` and `2`
  open Team and the combined task browser/inspector, while `Tab` toggles them.
  `e` opens session events as a temporary full-screen diagnostic viewer rather
  than a permanent tab. The task workspace provides structured details and a
  keyed task-owner activity view for role-level debugging. Tasks render newest
  first, and lowercase `r` creates an audited, correlated attempt for the
  selected terminal or blocked task without rewriting history. Root and
  delegated-task events use short-ID tree connectors to
  preserve hierarchy. Connector routes carry no text; card state, marker shape,
  color, line style, and animation communicate delegation without
  collision-prone labels.
- Launch and resume create no implicit objective. Users create work explicitly
  with `/task <objective>` or capital `R`. `R` creates a persisted Quick Run
  root task that asks the Team Lead to reconcile Markdown requirements, create
  bounded work, delegate it, execute it, and validate evidence. Delegation
  remains bounded by enabled membership, allowed targets, approvals, and task
  limits.
- Root README now provides local and heterogeneous-team onboarding, secure
  credential setup, TUI controls, and contributor quality commands.
- `voff init` interactively creates a validated solo Ollama or OpenAI
  configuration, supports non-interactive arguments, keeps credentials
  outside project files, and refuses accidental overwrite.
- Tag-triggered release CI builds locked Linux x86-64 and macOS Intel/Apple
  Silicon archives, generates SHA-256 checksums, and publishes them only for
  version tags. The maintainer procedure is documented in `docs/RELEASING.md`.
- A bare `voff` launch reopens the newest task-bearing session, while explicit
  `voff run` creates a clean session by default and `--resume` reopens history.
  The global `--workspace <DIR>` override selects that directory's local state
  without rewriting configuration. Initial TUI projections and interruption
  recovery remain scoped to the selected session.
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
