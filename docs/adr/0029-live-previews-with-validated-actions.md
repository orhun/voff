# 0029: Live generation previews with a separate action boundary

Status: accepted; supersedes the non-streaming transport decision in ADR 0028.

## Context

Waiting for the whole response hides progress, especially for long patches and
delegation plans. Requiring complete validated actions does not require delaying
read-only previews. Rig exposes text/tool fragments separately from its terminal
record, so replacing the SDK is unnecessary to provide live output.

## Decision

Use Rig streaming for OpenAI/Ollama. Immediately publish assistant text and
display-only tool argument deltas; ignore private reasoning and unknown payloads.
Keep actual tool calls buffered until the stream ends with a valid terminal
record, then validate and publish them through the existing complete-turn gate.
Never infer completion from usable partial text or retry after visible output.

The collector forwards previews over a bounded channel. The engine persists
them during generation without replaying them at completion. GenerationDelta
is a distinct canonical event and cannot become a tool proposal/task result.
Failed/cancelled tasks retain their partial output with their actual status.
TUI coalescing respects task identity when concurrent streams interleave.

## Limits

Rig remains an adapter detail; its decoder errors still fail explicitly rather
than approving incomplete actions. Token delivery follows provider chunking,
not synthetic character animation. CLI adapters retain their tested structured
completion path; true Codex token deltas require a separate app-server protocol
adapter, while Claude needs stream-json partial events. Neither capability is
claimed or simulated here. Existing configs and stored event schemas need no
migration. Old binaries cannot interpret the new GenerationDelta event.
