# Rig refactor: Phase 2 adapter bridge

Date: 2026-09-14

> Historical record: Phase 3 subsequently made Rig the sole production path.
> See `03-production-cutover.md` for the current operating model.

## Outcome

AgentMux now has private, production-compilable Rig adapters for OpenAI and
Ollama. They remain behind the `rig-provider` Cargo feature and an explicit
`AGENTMUX_PROVIDER_BACKEND=rig` trial switch. The default binary continues to
use the characterized legacy adapters, so this phase does not silently migrate
users or alter persisted provider DTOs.

## Boundary and behavior

- Rig is confined to `agentmux-providers`; core, engine, storage and TUI expose
  only AgentMux provider-neutral types.
- `CanonicalRequest` messages, tools and output limits are converted inside the
  adapter. Provider tool calls become `StructuredToolRequest` values and still
  traverse AgentMux policy, approval, audit and executor boundaries.
- OpenAI dotted tool names use the existing reversible `.` / `__` codec.
- Text, completed tool calls, usage and terminal response data become the
  existing canonical stream events. A successful stream emits exactly one
  `Completed`; EOF without a terminal response is `OutputTruncated`.
- Cancellation races request startup, stream reads and bounded event delivery.
  The Rig stream is cancelled and no successful terminal is synthesized.
- Telemetry content recording is disabled. OpenAI is restricted to the official
  HTTPS endpoint and Ollama to loopback endpoints.

## Error normalization

The provider-neutral taxonomy now distinguishes authentication, authorization,
quota, rate limit, timeout, transport, invalid request, context length,
unsupported capability, malformed response, tool-protocol, truncated output,
cancelled, transient and permanent failures. Rig mapping is centralized and
does not persist raw response bodies or credentials. Only the explicitly
retryable transport/rate-limit/timeout/transient/truncation classes can pass the
existing bounded resilience policy, and never after a stream event was emitted.

## Trial commands

The feature and runtime switch are both required:

```bash
AGENTMUX_PROVIDER_BACKEND=rig cargo run -p agentmux-cli \
  --features rig-provider -- doctor --config .agentmux/config.toml

AGENTMUX_PROVIDER_BACKEND=rig cargo run -p agentmux-cli \
  --features rig-provider -- run --config .agentmux/config.toml \
  --objective "Run a bounded provider-adapter trial"
```

Without the Cargo feature, selecting `rig` fails with an actionable error.
Unset the environment variable, or set it to `legacy`, to use the default path.
No configuration or database migration is involved.

## Offline verification

- Both default and `rig-provider` CLI compositions compile.
- Request conversion preserves messages, tool schemas, model and token limit.
- Stream fixtures verify ordered text/tool/usage/completion conversion and one
  success terminal.
- Provider-wire fixtures feed OpenAI Responses SSE and Ollama NDJSON through
  their concrete Rig models and prove the same canonical text/usage/completion
  sequence. Separate fixtures normalize both model-listing formats.
- EOF without a final response, invalid tool schema, unsafe endpoints and empty
  credentials are rejected.
- A capacity-one event channel proves cancellation completes within 100 ms even
  when the consumer is stalled, without emitting a success terminal.
- Table-driven response fixtures distinguish quota, context length,
  unsupported capability and tool protocol failures while keeping provider
  bodies out of the surfaced error message.
- Phase 1 still verifies both concrete client surfaces, bounded stream
  cancellation, tool-result correlation and architectural type isolation.
- The backend parser verifies the legacy default, both accepted values and
  rejection of an unknown value without mutating process environment in tests.

These fixtures are deterministic and make no network calls. Live credentialed
provider smoke tests remain deliberately opt-in and outside CI. On 2026-09-14,
both opt-in tests passed using local Ollama 0.32.15 with `qwen3:4b` and OpenAI
with `gpt-5-mini`. Both requests included the canonical `file.read` tool schema;
the prompt instructed the model not to invoke it, proving request acceptance
without causing a tool side effect or bypassing AgentMux approval.

## Measured packaging cost

Measured on the same macOS host with release profile:

| Build | Binary bytes | Provider normal/build tree lines |
|---|---:|---:|
| Default legacy build | 10,749,792 | 234 |
| `rig-provider` build | 16,858,288 | 371 |
| Rig trial delta | +6,108,496 (+56.8%) | +137 |

The increase is primarily the temporary coexistence of the legacy reqwest 0.12
and Rig reqwest 0.13 stacks. Keeping Rig opt-in prevents that cost from entering
the default beta artifact while parity work continues.

## Exit assessment

Phase 2 is complete. The private production adapter compiles, both provider wire
formats and model lists pass offline fixtures, detailed errors are normalized,
slow-consumer cancellation is bounded, and both opt-in live smoke tests pass.
Existing engine tests continue to prove that every canonical `ToolRequest`
enters durable `WaitingApproval` before execution, so translating a Rig tool
call cannot grant authority.

Completion of this phase does **not** authorize legacy deletion or make Rig the
default. Phase 3 must route broader characterization scenarios through the Rig
path, verify session/restart and TUI responsiveness at composition level, then
decide whether to accept the measured dependency cost. Until that gate passes,
`legacy` remains the default and rollback is removal of the feature/switch path.
