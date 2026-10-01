# Provider Adapter Development

Provider adapters translate an external model API into voff's canonical
contract. OpenAI request construction and Ollama streaming use `rig-core`, but Rig and other
provider-specific types must remain inside the `voff-provider` adapter module;
the engine depends only on `ModelProvider`.

Use Rig's low-level completion model directly. Do not use an SDK-owned agent
loop or allow the provider layer to execute tools; tool requests must return to
voff for policy evaluation and approval.

OpenAI uses a small Responses SSE decoder over `reqwest`/`eventsource-stream`,
accepting additive protocol metadata without requiring the entire response to
match an SDK enum. Ollama uses Rig's streaming completion model. Text and structured-output
fragments reach the UI immediately; `OutputDelta` is only a generation preview,
never a tool request or a completed task result. The adapter requires a valid
terminal record and checks tool authority, argument shape, required tool choice,
and finish reason before releasing tool requests. No partial-output salvage or
weaker fallback request is permitted. The engine separately gates all actions
on successful collection within the task deadline (4 MiB/4096 structural events;
adjacent text deltas are coalesced). Failed requests retain visible partial text
but cannot publish approvals. Cancellation drops the owned stream.

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

The `voff_provider::contract` module provides:

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
individual adapters. Each attempt buffers actionable events until success;
only display-only previews escape early. Transient connection loss, timeouts,
and rate limits can retry (three attempts by default), including after a preview.
An explicit `Retrying` event clears the previous attempt's response buffer;
failed-attempt actions are discarded. Backoff is exponential with bounded jitter.
Invalid task output does not trip a shared provider circuit breaker. Retries can
consume additional inference usage; failed-attempt usage may be unavailable.
Tools themselves are never replayed by provider retries.

## Installed CLI adapters

Codex and Claude CLI routes are process adapters, not Rig transports. They use
the installed binary's existing login and never require an API key. Launch them
without a shell, clear inherited API-key variables, isolate their working
directory, request schema-validated JSON, bound captured output, and translate
only proposed text or tool calls into the canonical provider contract. Voff's
engine still validates and approval-gates every delegation or side effect.

Codex uses `app-server --listen stdio://`, not `exec --json`. It initializes one
ephemeral, read-only thread with approvals disabled, streams agent-message
deltas, and requires a completed structured final message. Inherited hooks,
native tools, MCP servers and plugins are disabled through process-only
overrides and the effective config is verified before starting a task. All
server-originated tool/approval RPCs fail closed. This is not a claim of OS-level
filesystem isolation. The process is killed and reaped after the turn.

Claude uses `--output-format stream-json --verbose --include-partial-messages`
with restricted/safe mode, native tools disabled, no permission prompts, and no
session persistence. It requires a successful result envelope containing
`structured_output`. The installed CLI must support these flags (including
`--permission-prompts`, introduced in 2.1.259); incompatible versions fail during
capability probing with an update instruction. Safe mode retains login but can
still apply organization-managed settings; managed policies must not enable
side effects outside Voff's approval boundary. Neither adapter emits reasoning.

Process pipes are drained concurrently. JSONL lines and stderr are limited to
2 MiB; total protocol output is bounded (Codex 16 MiB, Claude 32 MiB). Processes
have a 300-second ceiling (or shorter task deadline), and cancellation drops/kills
the child. A CLI response can propose one tool call; a delegation call can
contain multiple proposals for different roles.

The opt-in `local_codex_delegates_and_applies_two_files` integration test makes
three real requests using installed Codex login, verifies live generation before
approval, approves only expected actions
in a disposable workspace, checks both files and task evidence, and reopens
SQLite to verify persistence. Run it with:

```bash
cargo test -p voff-engine --test local_codex -- --ignored --nocapture
```

It requires a compatible installed, logged-in Codex and consumes account usage.
Set `VOFF_TEST_CODEX_MODEL` to override the test model. Ordinary tests remain
offline and do not execute this test.

## Review boundary

Adding a provider changes the external data boundary and requires tests,
configuration documentation, a security review, and an ADR when it introduces
new protocol or capability behavior. Do not log raw prompts merely to simplify
debugging; use the redacted diagnostic export and synthetic fixtures.
