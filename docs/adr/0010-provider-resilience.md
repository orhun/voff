# ADR 0010: Provider resilience boundary

## Status

Accepted

## Decision

Retry and circuit-breaking behavior wraps the provider-neutral port rather than being implemented independently by adapters. Authentication, authorization, invalid-response and cancellation failures never retry. Rate-limit, timeout and internal failures retry only when the adapter explicitly marks them retryable and no stream event has been emitted.

Retries use bounded exponential backoff. Consecutive failures open a time-bounded circuit and later allow a half-open probe. Health exposes aggregate state without exposing request bodies or credentials.

## Consequences

Adapters remain focused on protocol translation. A partial model stream is surfaced as a failed attempt and is never replayed automatically, avoiding duplicate text, tool proposals or side effects.
