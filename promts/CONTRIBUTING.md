# Contributing to AgentMux

Thank you for helping build a safe, open and provider-neutral AI agent workspace.

## Before contributing

- Read `DESCRIPTION.md`, `AGENTS.md` and relevant documents under `docs/`.
- Search existing issues and discussions.
- For large features, provider adapters, protocol changes or security-sensitive work, open a design issue before implementation.
- Keep changes within the current roadmap phase unless maintainers approve otherwise.

## Development expectations

- Use stable Rust and the repository-pinned toolchain.
- Add tests for new behavior and meaningful failure paths.
- Keep provider-specific code out of the domain core.
- Do not include real credentials, customer data or proprietary prompt/output samples.
- Update configuration examples and migrations when schemas change.
- Add an ADR for material architectural decisions.

## Pull request checklist

- [ ] Scope is focused and linked to an issue/requirement.
- [ ] Format, clippy and tests pass.
- [ ] Security and privacy implications were considered.
- [ ] New permissions are explicit and least-privileged.
- [ ] User-facing behavior and config are documented.
- [ ] Breaking changes and migrations are identified.
- [ ] Provider tests can run with mocks and no paid credentials.

## Provider adapters

An adapter must:

- Implement the canonical provider contract.
- Declare capabilities and limitations.
- Normalize streaming, errors, cancellation and usage.
- Pass the shared contract test suite.
- Avoid leaking provider SDK types into core.
- Document credential references and data sent to the provider.

## Commit and review guidance

Prefer small, descriptive commits. Reviewers should prioritize correctness, safety boundaries, recovery, compatibility and tests over stylistic preference.

## Reporting security issues

Do not open a public issue for an exploitable vulnerability. Follow the private reporting process that will be published in the repository root security policy before public release.

## Code of conduct and license

Before accepting public contributions, maintainers must add a code of conduct, contributor license guidance if needed and a final OSI-approved project license.
