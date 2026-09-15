# Rig refactor: Phase 5 optional integrations and migration

Date: 2026-09-14

## Outcome

Phase 5 completes the scoped Rig migration. MCP and effect/replay were evaluated
against the selected 0.42.0 release and were not added to production. User and
contributor documentation now describes the single Rig provider path,
configuration compatibility, troubleshooting, upgrade and rollback.

## MCP decision

The planned standalone `rig-rmcp` package does not exist at version 0.42.0.
That release exposes MCP as the native-only `rmcp` feature of `rig-agent`; it
uses RMCP 2, owns a live tool registry, refreshes/disconnects tools and dispatches
them through the Rig agent runner. This is useful infrastructure but does not
provide AgentMux's policy and approval boundary.

Production adoption is rejected for this migration because discovered tools
are untrusted capabilities rather than grants, Rig dispatch would introduce an
alternate executor, metadata/results need AgentMux redaction and audit, and
connection ownership must integrate with session/task cancellation.

No `rmcp` package or feature is present in the lock graph. A future experimental
adapter must translate discovery into disabled AgentMux tool descriptors and
route every invocation through schema validation, policy, approval, executor,
audit and redaction before enabling any server.

## Effect and replay decision

No production-grade Rig effect/replay companion matching 0.42.0 is published.
The crates.io `rig-effect-log` entry is version 0.0.1 and identifies itself as a
placeholder, so it is not adopted. AgentMux keeps SQLite domain state,
canonical audit events and provider test fixtures as three separate concepts.
Offline OpenAI/Ollama fixtures satisfy replay testing without creating a second
durable production event log.

## Enforced boundary

Characterization tests assert that `rig-agent` remains test-only with default
features disabled, `rmcp` is not enabled, no effect-log package enters
`Cargo.lock`, and Rig agent state does not cross into core, engine, tools,
executor, storage, TUI or CLI. Existing engine tests prove unknown/malformed
tool requests cannot bypass approval or create side effects.

## Migration completion

- Rig 0.42.0 is exactly pinned.
- OpenAI and Ollama use one production adapter path.
- Legacy HTTP/SSE/NDJSON implementations and temporary switches are gone.
- Config version 1 and provider keys remain compatible.
- SQLite stores only AgentMux DTOs; an old session fixture opens successfully.
- Release binary remains 14,933,504 bytes on the Phase 3/5 measurement host.
- Rollback is a release rollback and requires no config/database migration.

See `docs/MIGRATING_TO_RIG.md`, `docs/PROVIDER_ADAPTERS.md` and
`docs/TROUBLESHOOTING.md` for operational guidance.

