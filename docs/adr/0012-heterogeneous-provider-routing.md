# ADR 0012: Heterogeneous provider routing

## Status

Accepted

## Decision

AgentMux supports a registry of named provider routes. Each persisted `AgentProfile` selects one route and one model, and the engine resolves the task owner's route immediately before provider execution. The canonical request, tool boundary and scheduler remain provider-neutral.

Configuration schema version 1 remains backward compatible: when `[[providers]]` is absent, the existing `[provider]` block is the only route. Heterogeneous formations declare named `[[providers]]` entries and set `provider` and optionally `model` on each `[[agents]]` entry. Provider IDs are validated before session startup.

Each route receives its own resilience wrapper and therefore its own retry/circuit state. A provider failure marks only that route's task failed; bounded scheduler siblings are awaited and may complete normally. Credentials remain environment references and are never embedded in agent profiles.

## Consequences

- Ollama and OpenAI tasks may execute in the same bounded scheduler wave.
- Existing solo and single-provider formation files continue to load unchanged.
- Provider/model identity is visible for every agent in the TUI.
- Pricing remains globally configured until per-route pricing is introduced.
