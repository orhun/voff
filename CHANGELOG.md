# Changelog

All notable changes to voff are documented here. This project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Renamed the project, CLI, crates, release artifacts, and workspace state
  directory to `voff`.
- Consolidated project documentation and removed bootstrap-only prompt files
  and local development configuration.
- Reduced the public CLI to workspace launch, `doctor`, and `export`. Normal
  launch resumes automatically; `--new-session` starts clean. The redundant
  `init`, `run`, and `sessions` commands and command-specific `--config`,
  `--resume`, and `--session` flags were removed before stable release.
- Fixed local Codex requests by emitting a fully strict output schema and
  encoding proposed tool arguments as validated JSON strings. Local CLI
  failures now map bounded stderr to safe authentication, schema, rate-limit,
  compatibility, or model guidance instead of reporting only exit status 1.
- Made the Team Lead composer task-only, moved team management to `t` and task
  cancellation to `x`, and added setup-style `c` provider/model configuration
  for the selected role. Team cards now show bounded provider-response
  excerpts, hide internal delegation prose, and label CLI-managed model
  selection clearly.
- Fixed broad Team Lead objectives stopping after one delegated role. One
  bounded structured plan now expands into separate per-role approvals and
  concurrently schedulable child tasks while retaining legacy single-proposal
  compatibility.

### Added

- Solo and multi-agent onboarding templates.
- Explicit session listing and selection.
- Provider adapter guide and reusable contract assertions.
- Task-graph property checks, malformed-input fuzz corpora, and large-history
  benchmarks.
- Credential-free Codex CLI and Claude CLI providers. Existing version-1
  configurations need no migration; the new `codex_cli` and `claude_cli` kinds
  are additive.

## [0.1.0-beta.1] - Unreleased

- Provider-neutral Rust orchestration engine and Ratatui interface.
- Ollama and OpenAI adapters with streaming, tools, usage, cancellation, retry,
  circuit breaking, and capability discovery.
- Persistent single- and multi-agent task graphs with approvals, scoped patch
  and test tools, redacted diagnostics, and crash recovery.
- Linux x86-64 and macOS x86-64/aarch64 release archives with SHA-256 files.

[Unreleased]: https://github.com/voffsh/voff/compare/v0.1.0-beta.1...HEAD
[0.1.0-beta.1]: https://github.com/voffsh/voff/releases/tag/v0.1.0-beta.1
