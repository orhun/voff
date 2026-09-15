# Rig refactor: Phase 0 current state

Date: 2026-09-14

This document records the pre-refactor AgentMux baseline. It is evidence for a
future incremental migration; Phase 0 does not add Rig or change production
provider behavior.

## Repository and worktree

- Workspace: Rust 2024, MSRV 1.91, ten crates, 9,422 Rust source lines.
- The worktree was already dirty with the completed Phase 5A/5B work. Those
  changes are preserved and are part of this measured baseline.
- No root `CLAUDE.md` exists. Binding instructions resolve through root
  `AGENTS.md` to `promts/AGENTS.md`; the root `CODEX.md` resolves similarly.
- Rig does not occur in the current manifests or source dependency graph.

## Crate responsibility matrix

| Crate | Current responsibility | Rig migration disposition |
|---|---|---|
| `agentmux-core` | IDs, tasks/DAG, charter/team/instances, claims, approvals, artifacts, canonical audit events | Keep Rig-free |
| `agentmux-provider` | Canonical request/event/error port; resilience, retry and circuit state; contract helpers | Keep as anti-corruption boundary |
| `agentmux-providers` | Mock plus concrete OpenAI/Ollama HTTP clients, wire DTOs and stream parsers | Primary Rig migration target |
| `agentmux-engine` | Dispatch, routing, scheduler, delegation, budgets, cancellation, approvals, handoff/review | Keep; consume only AgentMux provider port |
| `agentmux-tools` | Scoped read/search/create/patch proposals and digest/path checks | Keep |
| `agentmux-executor` | Approval-bound allowlisted process execution | Keep |
| `agentmux-storage` | In-memory/SQLite repositories, schema versions 1–5, events/usage/claims | Keep |
| `agentmux-config` | Secret-free versioned TOML and validation | Keep; provider factory inputs may feed adapter |
| `agentmux-tui` | Ratatui projection, keyboard actions, approvals and bounded event UI | Keep provider-neutral; remove unused concrete-provider dependency if proven safe |
| `agentmux-cli` | Composition, provider factory, init/doctor/run/sessions/export | Keep; compose Rig adapter here |

## Verified capability matrix

| Capability | Status and evidence |
|---|---|
| Headless shared engine | Implemented; engine has no Ratatui/SQLite/HTTP dependency |
| Ratatui TUI / terminal-first | Implemented in `agentmux-tui`; bounded Tokio channels |
| OpenAI + Ollama in one team | Implemented through named provider registry and owner route lookup |
| Solo and multi-agent | Implemented by init/config composition and formation snapshots |
| Streaming and cancellation | Implemented by `ProviderEventSink` plus `CancellationToken` selects |
| Scoped read/search and approval-gated create/patch/test | Implemented; tool/executor tests cover denial and path/digest constraints |
| Delegation, DAG, bounded waves, handoff and review | Implemented and covered by engine tests |
| SQLite, restart graph persistence and session selection | Implemented; repository reopen and CLI recovery tests |
| Retry/circuit/health/cost budgets | Implemented in provider-neutral resilience and engine budget layers |
| CLI init/doctor/run/sessions/export/resume/session | Implemented in `agentmux-cli` |
| Provider cassette/live smoke tests | Not implemented; current adapter parsing uses offline JSON unit fixtures |
| Rig adapter/runtime | Not implemented |
| Tauri/desktop | Planned; intentionally outside this refactor phase |

## Current provider flow

```text
Config + environment credential
  -> CLI provider factory
  -> concrete OpenAiProvider/OllamaProvider (reqwest client reused per route)
  -> ResilientProvider (retry/circuit/health)
  -> AgentMux ModelProvider
  -> Engine CanonicalRequest
  -> bounded ProviderEvent channel
  -> canonical audit event + SQLite + TUI projection
```

### OpenAI code-level trace

1. CLI resolves the configured environment-variable name and constructs one
   `OpenAiProvider`, then wraps it in `ResilientProvider`.
2. The adapter converts AgentMux messages/tools into a Responses API JSON body,
   normalizes strict schemas and maps dotted tool names reversibly.
3. A shared `reqwest::Client` posts `/v1/responses` with `stream=true` and
   `store=false`; SSE `data:` lines are parsed into text, tool, usage and
   completion events.
4. HTTP/network and incomplete-response failures map into AgentMux errors;
   cancellation races both request send and stream reads.

### Ollama code-level trace

1. CLI constructs one loopback-only `OllamaProvider`, then wraps it in the same
   `ResilientProvider`.
2. `models()` reads `/api/tags`; `start()` maps messages/tools to `/api/chat`.
3. NDJSON chunks map content, tool calls, prompt/evaluation usage and the done
   marker into the same AgentMux event vocabulary.
4. The same cancellation token races request send, stream reads and bounded
   event sends. Non-loopback endpoints are rejected at construction.

## AgentMux type to Rig candidate map

| AgentMux type/area | Rig candidate | Boundary decision |
|---|---|---|
| `CanonicalRequest` / `CanonicalMessage` | Rig completion request/messages | Convert only inside adapter |
| `ToolDefinition` / `StructuredToolRequest` | Rig tool definitions/call content | Translate; never execute through Rig directly |
| `ProviderEvent` / `ProviderCompletion` | Rig streaming items/completion response | Normalize into existing events |
| `ProviderErrorKind` | Rig completion/provider errors | One table-driven mapping module |
| `OpenAiProvider` wire/SSE code | Rig OpenAI model/client | Replace after parity |
| `OllamaProvider` wire/NDJSON code | Rig Ollama model/client | Replace after parity |
| `ResilientProvider` | No automatic replacement | Keep AgentMux retry/circuit semantics |
| Task graph/AgentInstance/Session | Rig AgentRun is evaluation-only | AgentMux remains source of truth |
| Canonical audit log | Rig effect/cassette facilities | Do not make a second production source of truth |

## Code to move versus code to retain

| Move behind Rig | Remain in AgentMux |
|---|---|
| OpenAI/Ollama request DTO construction | Provider-neutral port and errors |
| HTTP client/model construction | Retry, rate-limit policy and circuit breaker |
| SSE/NDJSON response decoding where Rig has parity | Cancellation deadline and task lifecycle |
| Provider completion/stream/tool wire conversion | Tool registry, policy, approval and safe executor |
| Provider model listing where the selected version supports it | Team routing, DAG, scheduler, handoff/review |
| Optional offline provider fixtures | Audit, redaction, SQLite, CLI and TUI |

Legacy candidates are the concrete OpenAI/Ollama reqwest fields, wire structs,
`openai_request_body`, `openai_events`, provider-specific HTTP error helpers and
Ollama NDJSON loop. None may be removed until Rig parity tests pass and the
production switch is complete.

## Baseline quality and performance

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Pass |
| `cargo test --workspace --all-features` | Pass: 87 unit tests, 0 failures; all doc-tests pass |
| `cargo deny check` | Pass: advisories/bans/licenses/sources; duplicate-version warnings only |
| Warm release build | 11.06 s wall, 49.39 s user, 1.86 s sys |
| `target/release/agentmux` | 10,752,240 bytes (macOS host) |
| SQLite large-history benchmark | 10,000 events: 64 ms write, 9 ms read (single macOS host run) |
| Duplicate groups | `getrandom`, `hashbrown`, `rustix`, `syn`, `unicode-width`, `windows-sys` |

The persistence benchmark supports 10,000 events. Phase 0 records the single
host result above as a comparison baseline, not a stable performance threshold.
It must be rerun before and after dependency adoption under the same
build/profile conditions.

## Rig evidence and version candidate

As of the research date, docs.rs presents Rig 0.42.0 and documents the facade
split between portable `rig-core` contracts and optional `rig-agent`. The 0.41.0
release introduced that split as a breaking change, while the upstream main
manifest already references 0.42.0. Therefore 0.42.0 is only a Phase 1
evaluation candidate, not yet an approved dependency. Phase 1 must inspect the
published crate source, MSRV and exact features and must pin through Cargo.lock;
no floating Git branch is acceptable.

Primary research links:

- <https://docs.rs/rig/0.42.0/rig/>
- <https://github.com/0xPlaygrounds/rig/releases>
- <https://github.com/0xPlaygrounds/rig/tree/main/crates/rig-core>
- <https://github.com/0xPlaygrounds/rig/tree/main/crates/rig-agent>
- <https://github.com/0xPlaygrounds/rig/tree/main/crates/rig-rmcp>

## Risks and open questions

1. Rig's 0.41 split and subsequent 0.42 changes make API/serialization drift a
   direct maintenance risk.
2. Rig stream terminal semantics, tool-call IDs and usage timing may not match
   AgentMux's exactly-once event contract.
3. A Rig tool executor could bypass approval unless the adapter exposes only
   typed proposals.
4. Rig may add duplicate HTTP/TLS/schema dependencies and materially increase
   binary size or MSRV.
5. Ollama model listing, OpenAI Responses strict tools, incomplete output and
   provider error detail may have parity gaps.
6. Cancellation must be proven bounded; dropping a Rig stream is not assumed to
   cancel the underlying request.
7. The TUI currently declares a concrete `agentmux-providers` dependency even
   though source imports do not show provider construction; remove only after a
   dependency guard proves it unused.
8. SQLite reopen/migration tests exist, but there is no checked-in historical
   database fixture. Phase 1 characterization should add one before any change
   that could affect persisted provider events or usage data.

## Phase 1 implementation plan

1. Download and inspect published Rig candidate metadata/source without changing
   production composition; compare MSRV and Tokio/reqwest/serde/tracing trees.
2. Build a temporary or test-support spike for OpenAI, Ollama, unary,
   streaming, tools, cancellation, usage, listing and error access.
3. Measure minimal `default-features = false` configurations against the
   facade/default setup, including compile graph and binary size.
4. Select and lock an exact crates.io version (or a verified SHA only if a
   published version cannot satisfy parity).
5. Write the adoption ADR with anti-corruption boundary, upgrade policy,
   rollback and measured trade-offs.
6. Add characterization/architecture tests before routing production traffic.

Phase 1 exit gate: both provider spikes compile and pass offline tests, the
selected dependency/features are locked and measured, and no Rig type appears
in core, engine, storage or TUI public contracts.
