# Repository Instructions for AI Agents

## Mission

Build voff as a safe, provider-neutral, terminal-first multi-agent orchestration system in Rust. Preserve the user's control over delegation, commands, files, cost, and final integration.

## Required reading order

Before changing code, read:

1. `DESCRIPTION.md`
2. `docs/PRODUCT_REQUIREMENTS.md`
3. `docs/ARCHITECTURE.md`
4. `docs/AGENT_PROTOCOL.md`
5. `docs/CONFIGURATION.md`
6. `docs/SECURITY.md`
7. `docs/ROADMAP.md`

If a more deeply nested `AGENTS.md` exists, its rules supplement or override this file for that subtree.

## Non-negotiable principles

- Use stable Rust unless a documented requirement needs nightly.
- Keep the orchestration core independent from Ratatui and provider SDKs.
- Treat providers, tools, storage engines and execution backends as adapters.
- Never hard-code Codex, Claude, Gemini or any role into domain logic.
- Model delegation as explicit, persisted tasks; never as hidden prompt-only behavior.
- Require approval for destructive, privileged, external side-effect or policy-sensitive actions.
- Never log API keys, authorization headers, full secret-bearing prompts or raw environment dumps.
- Do not silently broaden filesystem, shell, network or delegation permissions.
- Prefer deterministic state transitions and structured messages over parsing free-form prose.
- Implement only the active roadmap milestone unless the user explicitly expands scope.

## Engineering workflow

1. Restate the requested outcome and identify the relevant acceptance criteria.
2. Inspect existing code and tests before proposing structural changes.
3. Record material assumptions in the issue, plan or relevant ADR.
4. Make the smallest coherent change that satisfies the current milestone.
5. Add or update tests at the same abstraction level as the change.
6. Run formatting, linting and relevant tests.
7. Report changed behavior, verification, risks and remaining work.

## Rust quality gate

For a normal code change, run when available:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

Do not claim success if a check was skipped or failed. Explain environmental blockers precisely.

## Architecture rules

- Domain types and state machines belong in `voff-core`.
- Application orchestration belongs in `voff-engine`.
- Provider-specific translation belongs only in provider adapter crates.
- Terminal rendering and input handling belong in the `voff` application crate's `tui` module.
- Shell/process execution belongs behind the executor interface in `voff-tools`.
- SQLite persistence belongs behind repository traits.
- Cross-crate dependencies must point inward toward stable abstractions.
- Avoid a global mutable application state. Use explicit ownership and message passing.
- Use bounded channels and cancellation tokens for long-running work.

## Task and delegation rules

- Every executable task must have a unique ID, owner, objective, acceptance criteria and status.
- A child task must reference its parent and cannot create a cycle.
- Delegation depth, child count, concurrency and budget must be enforced by the engine.
- An agent may propose delegation without having authority to dispatch it.
- Task completion requires a structured result and evidence; prose such as “done” is insufficient.
- Failed, blocked and cancelled are terminally meaningful states and must not be rewritten as completed.
- The integrating agent must detect overlapping file claims or diffs before merge/application.

## Security rules

- Commands are data: parse, classify, display and authorize them before execution.
- Default to workspace-scoped file access and denied network access.
- Store secrets via environment references or OS secret facilities, never plaintext project config.
- Redact secrets from events and UI views.
- Persist an audit event for approvals, denials, tool calls and permission changes.
- Tests must include denial paths, cancellation, malformed provider output and restart recovery.

## UI rules

- Keyboard-only operation must remain possible.
- The UI must stay responsive while providers or commands run.
- Never perform blocking provider, storage or process work on the render loop.
- Colors cannot be the only carrier of status information.
- Support narrow terminals and provide a plain-text/error fallback.
- A user must be able to pause, cancel and inspect any active task.

## Documentation rules

- Public types and configuration fields require clear documentation.
- User-visible config changes require examples and migration notes.
- Architectural changes require an ADR under `docs/adr/`.
- Provider capability limitations must be documented, not hidden behind generic errors.

## Prohibited shortcuts

- No regex-only parsing of arbitrary model output for critical actions.
- No automatic execution of commands embedded in normal assistant text.
- No unbounded recursive delegation or retry loops.
- No shared write access by concurrent agents without conflict control.
- No provider-specific types crossing core boundaries.
- No placeholder security that is described as production-ready.
