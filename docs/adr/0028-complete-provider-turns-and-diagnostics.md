# 0028: Complete provider turns and auditable request lifecycles

Status: accepted

## Context

SSE terminal-frame decoding failures were being salvaged after partial tools
had reached orchestration. Warnings appeared as failures, overlapping scheduler
calls could dispatch the same work, and CLI pipe handling could stall requests.

## Decision

Keep Rig for OpenAI/Ollama, using its complete-response API and canonical
translation. Remove streaming recovery and its duplicate fallback request path.
Validate response termination and tool output before publishing it. Collect
every provider turn behind bounded event/byte limits and a task deadline, with
owned futures so cancellation does not leave detached requests.

Retain login-based Codex/Claude process adapters. Drain both pipes while writing
stdin and waiting for exit; bound output, deadline and child lifetime. Tools
remain proposals evaluated by Voff, never executed by an SDK agent loop.

Serialize task starts, scheduler waves, parent reconciliation and approval
decisions at their respective boundaries. Children inside one wave still run
concurrently. Validate an entire delegation batch before approvals and resolve
targets against the active formation. Failed parents identify failed children.

Write metadata-only tracing diagnostics off the render thread to workspace-local
logs. Separate provider warnings from failure events. Keep SDK body/prompt traces
out of these files. Add an opt-in real Codex test exercising two workers,
approved patches, evidence and reopened persistence.

## Consequences and limits

Text is displayed after completion, trading token-by-token streaming for a
single reliable action boundary. A malformed/truncated response fails rather
than pretending partial output is complete. Existing configurations still work.
Retries remain bounded and cannot follow emitted output. Logs rotate on startup,
not during a running session; they are diagnostics, not a transactional audit
replacement. Serialization is per Engine instance, not a cross-process lock.
File conflict checks and human approvals remain necessary. Live cloud endpoint
availability and arbitrary model compliance cannot be guaranteed by tests.
