# ADR: Adopt Rig behind the AgentMux provider boundary

- Status: accepted and in production
- Date: 2026-09-14
- Scope: provider infrastructure only

## Context

AgentMux has working OpenAI Responses and Ollama adapters with its own HTTP,
wire parsing and streaming code. The product value and security boundary live
above those adapters: task DAGs, scheduling, delegation, approvals, controlled
tools, audit, persistence, recovery, budgets, CLI and TUI. Reimplementing common
provider mechanics increases maintenance cost, but replacing the AgentMux
domain or scheduler with a framework runtime would weaken its boundaries.

Phase 0 captured the pre-adoption state in
`docs/refactor/rig/00-current-state.md`. Phase 1 compiled the published Rig
source against AgentMux's actual MSRV and exercised its OpenAI Responses,
Ollama, unary, streaming, tool-correlation, cancellation, usage and model-list
surfaces without credentials or network calls.

## Decision

Adopt the crates.io release `rig-core = 0.42.0`, pinned exactly in the provider
adapter's dependency declaration and checksummed in `Cargo.lock`. Use
`default-features = false` with only `reqwest` and `rustls`. Do not add the root
`rig` facade, `rig-agent`, derive macros, MCP or effect-log crates at this stage.

Phase 1 introduced the dependency for characterization and Phase 2 added an
opt-in adapter for parity work. Phase 3 completed the production cutover:
OpenAI and Ollama now always use the Rig adapter, while the legacy transport,
feature flag and runtime switch have been removed. The dependency direction is:

```text
AgentMux CanonicalRequest / ModelProvider
  -> agentmux-providers::RigProviderAdapter
  -> rig-core client + concrete provider model
  -> AgentMux ProviderEvent / ProviderCompletion / ProviderError
```

Only `agentmux-providers` may depend on Rig. `agentmux-core`,
`agentmux-provider`, `agentmux-engine`, `agentmux-storage` and `agentmux-tui`
must not import or expose a Rig type. An automated manifest guard enforces this
initial boundary.

Rig owns provider client/model construction, provider wire translation,
completion/stream transport, normalized Rig message/tool values and provider
response metadata. AgentMux remains authoritative for provider descriptors and
capabilities, retry/circuit/health, cancellation deadlines, canonical events,
tool authorization/execution, task/session state, audit and persistence.

A Rig tool call is data, never authority. It must become an AgentMux
`StructuredToolRequest` and pass the existing schema, policy, approval,
executor, audit and redaction path. Rig's stream cancellation ends a stream
without a success terminal; the adapter must translate the external
`CancellationToken` into an explicit AgentMux cancellation result.

## Version and feature evidence

The published 0.42.0 manifests declare Rust edition 2024 but no `rust-version`.
The Phase 1 spike therefore compiled with the repository's real MSRV toolchain,
Rust 1.91.1. Both provider clients and their unary/streaming futures compile.

Direct `rig-core` avoids the facade's default `rig-agent` and derive features.
Disabling `rig-core` defaults also omits its derive feature; `reqwest` and
`rustls` are explicitly restored because both target providers require HTTP.
The spike exposed an important temporary cost: Rig 0.42.0 used reqwest 0.13
while the legacy adapters used reqwest 0.12. Phase 3 removed the legacy stack,
so reqwest 0.13 is now the sole HTTP transport.

Measured on the Phase 1 host:

| Measurement | Before/production graph | With test-only Rig spike |
|---|---:|---:|
| Unique `agentmux-providers` normal/build tree lines | 145 | 145 |
| Unique tree lines including dev dependencies | 145 | 221 |
| Release `agentmux` binary | 10,752,240 bytes | 10,752,240 bytes |
| Warm release build | 0.23 s | 0.23 s |
| Warm Rig spike suite | n/a | 6 offline tests (final gate records timing) |

The unchanged Phase 1 production graph and binary were expected because Rig was
test-only. Phase 2 measured the real opt-in adapter build: 16,858,288 bytes
versus 10,749,792 bytes for the default legacy build on the same host, a
6,108,496-byte (56.8%) increase. The provider normal/build dependency tree grew
from 234 to 371 unique rendered lines. That is historical Phase 2 evidence. The
single-stack Phase 3 binary is 14,933,504 bytes with 350 provider normal/build
tree lines; detailed evidence is in
`docs/refactor/rig/03-production-cutover.md`.

## Error boundary

Rig exposes structured completion variants plus provider response status/body
helpers. Phase 1 proves these are reachable through one candidate mapping seam.
Phase 2 expands AgentMux's error taxonomy before production routing so
transport, quota, invalid request, context length, unsupported capability, tool
protocol and transient/permanent provider failures are not collapsed into
`Internal`. Mapping is centralized and table-driven; scattered string matching
is rejected.

## Upgrade policy

- Pin an exact crates.io version and commit `Cargo.lock`.
- Upgrade one Rig minor version at a time in an isolated change.
- Read that release's source/release notes and rerun provider contract,
  cancellation, security, MSRV, dependency and binary-size gates.
- Do not use a floating Git dependency. A Git revision is allowed only when a
  documented blocking fix has no release, and must use a full commit SHA.
- Persist only AgentMux DTOs; Rig serialization changes must not invalidate old
  sessions.

## Rollback

Production rollback is performed at version-control/release level; there is no
permanent hidden dual stack or runtime switch. The cutover did not change the
database or configuration schema. Back up `.agentmux/agentmux.db` before any
future schema migration.

## Alternatives

- **Keep all custom provider wire code:** rejected because it retains recurring
  protocol maintenance once Rig parity is proven.
- **Use the root `rig` facade with defaults:** rejected for unnecessary
  `rig-agent`/derive surface and a larger dependency/upgrade boundary.
- **Replace AgentMux orchestration with `rig-agent`:** rejected because Rig's
  agent loop is not the authority for AgentMux DAG, approvals, budgets or
  persisted session state.
- **Track Rig `main`:** rejected because reproducible builds and rollback require
  an immutable release or SHA.
- **Adopt `rig-rmcp` or effect logging now:** deferred. Neither may bypass the
  tool policy boundary or replace AgentMux's canonical audit log.

## Consequences

The migration gains a maintained provider-normalization layer without coupling
the product domain to Rig. The production binary is larger than the former
legacy-only binary, but the custom provider wire code and duplicate reqwest
stack are gone. Offline parity, explicit error mapping, bounded cancellation,
security tests and the measured binary impact passed the production gate.

## Phase 2 validation addendum

The adapter-level gates are complete: concrete OpenAI SSE and Ollama NDJSON
fixtures normalize to the same AgentMux event sequence, model-list fixtures
pass, response errors use a centralized status/body-code seam, and a
capacity-one channel test proves bounded cancellation. Opt-in live tests passed
for `gpt-5-mini` and local `qwen3:4b`, including strict tool schemas. This is a
historical Phase 2 gate; Phase 3 subsequently completed production cutover.

## Phase 3 production addendum

Composition-level routing, failure isolation, approval, SQLite reopen and TUI
streaming tests passed. The CLI factory now has a regression test for the Rig
production route. Legacy OpenAI/Ollama HTTP and parser code, the feature gate,
and the runtime selector were deleted; the provider factory has exactly one
real implementation path. See `docs/refactor/rig/03-production-cutover.md` for
the release measurement and operational rollback procedure.

## Phase 4 agent runtime addendum

`rig-agent` 0.42.0 was evaluated with default features disabled. Its
serializable sans-I/O `AgentRun` is technically capable of resuming a suspended
model-turn protocol, but its `AgentRunner` also owns tool dispatch, memory and
the multi-turn driver. Adopting it would create a second authority beside the
AgentMux engine and approval executor. It is therefore retained only as an
exact-pinned development characterization dependency in `agentmux-providers`;
no Rig run state is persisted and no production crate outside the adapter
boundary depends on it. See
`docs/refactor/rig/04-agent-runtime-boundary.md`.

## Phase 5 optional integration addendum

The selected Rig release exposes MCP through the optional native `rmcp` feature
of `rig-agent`; it is not a provider-normalization requirement and its runtime
owns tool registration and invocation. Enabling it would bypass or duplicate
AgentMux's policy and approval authority, so MCP remains disabled. No compatible
production effect-log package was adopted; deterministic provider fixtures are
test evidence while SQLite AgentMux events remain the sole durable audit and
recovery record. See
`docs/refactor/rig/05-optional-integrations-and-migration.md`.
