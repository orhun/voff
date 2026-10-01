# ADR 0030: Streaming transports and isolated request retries

Status: Accepted

## Context

The user requires visible generation, reliable delegated requests, and local
CLI authentication without API keys. Full SDK response enums can reject
otherwise valid terminal messages when upstream adds metadata. Retrying after
publishing actionable output is unsafe, but refusing to retry after any visible
token makes recoverable connection failures unnecessarily terminal.

## Decision

Retain Rig for OpenAI request mapping/model discovery and Ollama streaming.
Decode the narrow OpenAI Responses SSE contract using reqwest and
eventsource-stream; ignore additive metadata while strictly validating terminal
status and executable function arguments. No alternate endpoint or weaker
unstructured fallback is used on failure.

Use Codex app-server JSONL for real message deltas and structured final output.
Verify process-only isolation overrides before creating a read-only ephemeral
thread; explicitly disable inherited integrations and reject server-originated
tool/approval calls. Use Claude's stream-json partial events with a successful
structured terminal envelope and capability-probed safety flags. Both preserve
installed CLI login, not inherited API-key variables.

Each resilience attempt buffers actions until the whole attempt succeeds.
Preview events are not execution authority. Bounded transient retries emit an
explicit reset boundary, discard all unsuccessful proposals, and use backoff
with jitter. Invalid output does not count as a provider-wide circuit failure.
Task deadlines, cancellation and bounded output still apply across the request.

## Consequences and limitations

- This supersedes the completion-only CLI and no-retry-after-preview decisions
  in ADRs 0028/0029. No config schema migration is required.
- There is no guarantee against upstream outages, quota limits or invalid model
  output. Failed requests remain failed after the retry budget is exhausted.
- Retries may consume additional inference usage; usage for failed attempts is
  not reliably available. Provider-native internal retries remain outside Voff's
  attempt accounting.
- Native CLI compatibility is checked, not silently worked around. Claude needs
  recent restricted-mode/permission-prompt flags. Managed enterprise settings
  remain an administrator-controlled boundary; this is not OS sandbox assurance.
- Actions still return to the engine for validation, persisted delegation and
  per-action approval; transport code never executes proposed tools.
- Tests cover fragmented HTTP SSE, schema drift, malformed/oversized JSONL,
  native RPC denial, retry action isolation, cancellation and live Codex
  two-role delegation with real streaming before approval. Live cloud/Claude
  verification requires separately available credentials/compatible binaries.
