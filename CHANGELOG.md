# Changelog

All notable changes to AgentMux are documented here. This project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Solo and multi-agent onboarding templates.
- Explicit session listing and selection.
- Provider adapter guide and reusable contract assertions.
- Task-graph property checks, malformed-input fuzz corpora, and large-history
  benchmarks.
- Exact-pinned Rig provider adapters, migration guidance, and provider
  troubleshooting documentation.

### Changed

- OpenAI and Ollama now use a single Rig-backed production implementation; the
  legacy custom HTTP/stream parsers and runtime backend selector were removed.

### Security

- Rig agent, MCP, and experimental effect execution remain outside production
  authority; AgentMux continues to own approvals, execution, and audit state.

## [0.1.0-beta.1] - Unreleased

- Provider-neutral Rust orchestration engine and Ratatui interface.
- Ollama and OpenAI adapters with streaming, tools, usage, cancellation, retry,
  circuit breaking, and capability discovery.
- Persistent single- and multi-agent task graphs with approvals, scoped patch
  and test tools, redacted diagnostics, and crash recovery.
- Linux x86-64 and macOS x86-64/aarch64 release archives with SHA-256 files.

[Unreleased]: https://github.com/sekipsemihmetin/Agentmux/compare/v0.1.0-beta.1...HEAD
[0.1.0-beta.1]: https://github.com/sekipsemihmetin/Agentmux/releases/tag/v0.1.0-beta.1
