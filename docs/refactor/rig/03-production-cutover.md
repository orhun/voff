# Rig refactor: Phase 3 production cutover

Date: 2026-09-14

## Outcome

Phase 3 is complete. OpenAI and Ollama production construction now has one
implementation: the private Rig adapters in `agentmux-providers`. The temporary
`rig-provider` Cargo feature, `AGENTMUX_PROVIDER_BACKEND` runtime switch, direct
legacy HTTP clients, SSE/NDJSON parsers and duplicate reqwest 0.12 stack were
removed. Existing `OpenAiProvider` and `OllamaProvider` names remain aliases so
downstream Rust code does not need a source migration; they do not select a
second implementation.

No configuration or database schema changed. Existing OpenAI/Ollama TOML files,
session records and canonical persisted events remain provider-neutral.

## Preserved boundaries

- Rig types and dependencies remain confined to `agentmux-providers`; a
  manifest contract test guards core, provider, engine, storage and TUI.
- Model output remains untrusted data. Tool calls are converted to AgentMux
  `StructuredToolRequest` values and still require the engine's policy,
  approval, executor, audit and redaction flow.
- Retry, health, circuit breaking, task state, scheduling, delegation,
  persistence and recovery remain owned by AgentMux.
- OpenAI credentials are environment-only, OpenAI uses its official HTTPS
  endpoint, and Ollama remains restricted to loopback.

## Composition evidence

The production provider factory has a regression test proving a real Ollama
configuration constructs the Rig-backed descriptor. The integration suite also
proves parallel heterogeneous routing, per-route failure isolation, durable
approval, SQLite graph reopen, TUI stream formatting, bounded slow-consumer
cancellation, and equal canonical results for OpenAI SSE and Ollama NDJSON
fixtures.

The opt-in live tests remain ignored in ordinary CI because one requires a
local daemon and the other may incur API cost. Both passed the Phase 3 final
live gate against `qwen3:4b` and `gpt-5-mini`.

## Dependency and packaging result

Measured on the same macOS host with the release profile:

| Build | Binary bytes | Provider normal/build tree lines |
|---|---:|---:|
| Legacy default (Phase 2 baseline) | 10,749,792 | 234 |
| Temporary dual-stack Rig build | 16,858,288 | 371 |
| Phase 3 single Rig production build | 14,933,504 | 350 |

Removing the legacy transport reduces the dual-stack build by 1,924,784 bytes
(11.4%) and 21 rendered dependency lines. The remaining increase over the
legacy baseline is 4,183,712 bytes (38.9%), accepted in exchange for deleting
the custom provider wire stack. `cargo tree -d` confirms there is no duplicate
reqwest version; Rig's reqwest 0.13 is the sole HTTP transport.

## Rollback and operation

There is no hidden runtime rollback switch and no permanent dual stack.
Rollback is a version-control/release rollback to the prior binary; because the
cutover changes neither configuration nor persisted DTOs, it requires no data
migration. A future Rig upgrade must follow the ADR's exact-version upgrade
policy and rerun all contract, live, MSRV, security and package-size gates.
