# Changelog

All notable changes to voff are documented here. This project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Renamed the project, CLI, crates, release artifacts, and workspace state
  directory to `voff`.
- Consolidated project documentation and removed bootstrap-only prompt files
  and local development configuration.

### Added

- Solo and multi-agent onboarding templates.
- Explicit session listing and selection.
- Provider adapter guide and reusable contract assertions.
- Task-graph property checks, malformed-input fuzz corpora, and large-history
  benchmarks.

## [0.1.0-beta.1] - Unreleased

- Provider-neutral Rust orchestration engine and Ratatui interface.
- Ollama and OpenAI adapters with streaming, tools, usage, cancellation, retry,
  circuit breaking, and capability discovery.
- Persistent single- and multi-agent task graphs with approvals, scoped patch
  and test tools, redacted diagnostics, and crash recovery.
- Linux x86-64 and macOS x86-64/aarch64 release archives with SHA-256 files.

[Unreleased]: https://github.com/voffsh/voff/compare/v0.1.0-beta.1...HEAD
[0.1.0-beta.1]: https://github.com/voffsh/voff/releases/tag/v0.1.0-beta.1
