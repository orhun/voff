# ADR 0011: OpenAI Responses API provider

## Status

Accepted

## Decision

The second network provider uses the OpenAI Responses API with SSE streaming. AgentMux maps canonical messages and strict function tools at the adapter boundary, translates text, function-call and usage events back into provider-neutral events, and leaves the engine unchanged.

The API key is read only from the configured environment-variable name (default `OPENAI_API_KEY`). It is never accepted as TOML data, persisted to SQLite, or included in diagnostics. The adapter permits only `https://api.openai.com`.

HTTP 401/403 failures are not retried. Rate limits, timeouts and server failures use the shared bounded resilience policy. Retries stop after any streamed event to prevent duplicate output or tool effects.

Model prices vary over time, so AgentMux does not embed price claims. Users may configure input/output rates per million tokens; the TUI labels the calculated value as an estimate and otherwise reports that cost is not configured.
