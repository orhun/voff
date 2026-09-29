# ADR 0024: Isolated local CLI providers

## Status

Accepted. Extends ADR 0015; it does not replace the Rig-backed OpenAI and
Ollama transports.

## Context

Users may already have authenticated Codex and Claude Code installations. Voff
should be able to use those local binaries without requiring API keys and
without letting their built-in agent loops bypass Voff's persisted tasks,
permissions, and approval boundaries.

## Decision

- Add exact-binary local routes for `codex exec` and `claude -p` alongside the
  existing Rig-backed providers.
- Use each CLI's existing authenticated session; never require or resolve an API
  key for these routes.
- Launch without a shell, clear inherited environment variables, restore only a
  small login/runtime allowlist, disable persistence, and run in an isolated
  temporary directory rather than the project workspace.
- Request schema-validated JSON with bounded process output. Local CLI output
  may describe text and proposed Voff tool calls only; it cannot authorize or
  execute a Voff tool.
- Claude runs with built-in and MCP tools disabled. Codex runs ephemeral and
  read-only; its CLI currently lacks an equivalent disable-all-tools flag, so
  that limitation remains visible in documentation and diagnostics.
- Keep retries and circuit breaking in `ResilientProvider`.

## Alternatives

- Require cloud API credentials even when an authenticated CLI is installed.
  This creates unnecessary setup and excludes local-login workflows.
- Run vendor CLIs with their full agent toolsets. That would create a second,
  unaudited executor beside Voff's approval-gated tools.
- Replace Rig-backed providers with hand-written HTTP adapters. This is outside
  the local CLI requirement and duplicates mature transport logic.

## Consequences

Codex and Claude CLI routes require an installed, authenticated compatible
binary but no API key. Their availability and capability limitations are
reported explicitly. OpenAI and Ollama continue to use the previous commit's
Rig-backed adapters unchanged.
