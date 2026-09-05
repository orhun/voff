# ADR 0006: Phase 1 real provider

## Context
The single-agent slice needs one real streaming provider without coupling core to a vendor or requiring paid credentials in tests.
## Decision
Add a local Ollama HTTP adapter using the documented [`/api/tags`](https://docs.ollama.com/api/tags) endpoint and NDJSON [`/api/generate`](https://docs.ollama.com/api/generate) streaming. Phase 1 accepts loopback endpoints only, supports text streaming, usage, and cancellation, and declares unsupported capabilities honestly. Mock remains the default and test provider.
## Alternatives Considered
Cloud APIs require secret handling and external test dependencies. An Ollama CLI wrapper provides weaker structured streaming and cancellation.
## Consequences
Users must install Ollama and a model for real runs. No model is downloaded automatically and no remote endpoint is allowed in Phase 1.
## Status
Accepted
