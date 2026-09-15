# Provider Adapter Development

Provider adapters translate an external model API into AgentMux's canonical
contract. Provider-specific request, response, SDK, and error types must remain
inside `agentmux-providers`; the engine depends only on `ModelProvider`.

The production OpenAI and Ollama adapters use the exact-pinned Rig provider
crates through one implementation path. Rig types remain private to this crate;
canonical AgentMux requests, events, errors, and usage records are the public
boundary.

## Adding a provider

1. Verify that the provider exists in the pinned Rig release and inspect its
   actual features, target support, and source-level behavior.
2. Define a stable descriptor and advertise only capabilities the adapter can
   prove.
3. Add validated configuration that stores only an environment-variable or
   secret-store reference, never the credential value.
4. Map canonical messages, tools, streams, usage, and errors inside
   `agentmux-providers`.
5. Run the shared contract suite and add deterministic provider wire fixtures.
6. Add a health/model probe; keep cost metadata optional and user-maintained.
7. Wire the adapter in CLI composition and doctor/TUI reporting without adding
   concrete-provider branches to the scheduler.

## Implementation checklist

1. Implement `ModelProvider::descriptor`, `models`, and `start`.
2. Declare capabilities conservatively. A capability is true only when the
   adapter implements and tests it.
3. Translate every request from `CanonicalRequest`; do not pass provider types
   into core or engine crates.
4. Stream normalized `ProviderEvent` values through the bounded sink.
5. Emit exactly one final `Completed` event on success and return matching usage
   when usage is available.
6. Observe the cancellation token during network waits and stream processing.
7. Map failures to the narrowest `ProviderErrorKind`. Authentication,
   authorization, invalid response, and cancellation errors are not retryable.
8. Never include credentials, authorization headers, or secret-bearing request
   bodies in errors or events.
9. Add configuration validation and `doctor` capability/connectivity reporting.
10. Register the adapter only in CLI composition; do not add provider branches
    to orchestration logic.

## Contract test support

Enable `agentmux-provider`'s `test-support` feature in the adapter crate's dev
dependencies:

```toml
[dev-dependencies]
agentmux-provider = { path = "../agentmux-provider", features = ["test-support"] }
```

The `agentmux_provider::contract` module provides:

- `request(model)` for a canonical probe request;
- `run(provider, request, cancellation)` for concurrently draining bounded
  events without deadlocking;
- `assert_descriptor` for stable provider identity requirements;
- `assert_failed_stream` for canonical error categories, retryability, and the
  guarantee that failed runs never emit completion;
- `assert_tool_request` for non-empty tool names and object-shaped valid JSON;
- `assert_successful_stream` for final-event and usage consistency rules.

Use a deterministic fake server or transport to test a real adapter. Contract
tests must not require paid credentials or public network access.

Live provider tests are opt-in smoke tests. Keep them ignored by default,
require explicit environment credentials, and never make them a substitute for
deterministic contract and wire-fixture coverage.

## Required adapter tests

- successful text streaming and completion;
- tool-call name and JSON argument mapping;
- usage mapping and completion consistency;
- cancellation before and during a stream;
- authentication and authorization classification;
- rate-limit/timeout retryability classification;
- malformed payload rejection as `InvalidResponse`;
- receiver closure without panic;
- secret-free errors and diagnostics;
- provider-specific schema normalization and capability limitations.
- pre-cancelled and in-flight cancellation;
- authentication, authorization, invalid-response, timeout, and rate-limit
  mappings, including retryability;
- malformed provider payloads and malformed tool arguments.

Resilience retries and circuit breaking belong in `ResilientProvider`, not in
individual adapters. A provider must not retry after emitting an event because
that could duplicate text or tool side effects.

## Review boundary

Adding a provider changes the external data boundary and requires tests,
configuration documentation, a security review, and an ADR when it introduces
new protocol or capability behavior. Do not log raw prompts merely to simplify
debugging; use the redacted diagnostic export and synthetic fixtures.

MCP-discovered servers, tools, schemas, and responses are untrusted input. Any
future MCP adapter must route calls through AgentMux validation, policy,
approval, timeout/cancellation, executor, audit, and redaction boundaries; a
provider runtime must never invoke a discovered tool directly.
