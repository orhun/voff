# Security Model

## 1. Trust boundaries

Treat model output, provider responses, repository content, tool output, imported configurations and plugin metadata as untrusted. The user and enforced local policy are the authority.

## 2. Threats in scope

- Prompt injection from files, web content or tool output
- Destructive or privilege-escalating commands
- Path traversal and writes outside workspace
- Secret leakage through prompts, logs, subprocesses or exports
- Delegation bombs, retry storms and budget exhaustion
- Concurrent write conflicts and stale patches
- Malicious provider/tool adapter behavior
- Terminal escape/control-sequence injection
- Dependency and supply-chain compromise

## 3. Permission model

Effective permission is the intersection of:

`administrator policy ∩ session policy ∩ agent policy ∩ task grant ∩ tool capability`.

An inner layer may restrict but cannot broaden an outer layer. Default-deny network and external side effects. Grants have scope and duration.

Approval scopes:

- Deny
- Allow once for exact action
- Allow for this task and constrained pattern
- Allow for this session and constrained pattern

Permanent rules require direct config editing; a transient dialog must not create them silently.

## 4. Command classification

Before shell execution, show and evaluate:

- Executable and arguments
- Working directory
- Environment variable names passed
- Expected filesystem/network effect
- Risk category and reason
- Timeout and output limit

Always require approval or deny operations involving destructive deletion, privilege escalation, system paths, credential stores, arbitrary remote scripts, publishing/deployment, financial actions or external communications unless an explicit higher-level policy safely authorizes them.

Do not rely only on string blocklists. Combine structured invocation, path resolution, sandbox boundaries, policy rules and human review.

## 5. Filesystem

- Canonicalize roots and targets before authorization.
- Prevent symlink escapes and `..` traversal.
- Deny sensitive paths such as `.env`, SSH keys and credential directories by default.
- Patch proposals include base digest and affected paths.
- Use atomic replacement for internal state files.
- Limit artifact and output size to prevent disk exhaustion.

## 6. Network

- Providers use explicit allowed endpoints.
- Agent tools have network disabled by default.
- Redirects, DNS rebinding and private-address access require defensive handling in any future web tool.
- Record destination host and authorization decision without sensitive query data.

## 7. Secrets and privacy

- Prefer environment references. During the ADR 0019 transition, interactive
  credentials may use the workspace-local SQLite table with mode `0600` on
  Unix. It is plaintext at rest and must not be presented as production-grade
  secret storage.
- Redact known secret values and sensitive headers before persistence/rendering.
- Avoid passing full environment maps to child processes.
- Diagnostic exports are redacted and previewable.
- Telemetry is off by default.
- Document what each provider receives.

## 8. Prompt injection defenses

- Delimit trusted project instructions from untrusted content.
- Label source and trust level in context items.
- Models cannot grant themselves permissions.
- Tool/delegation requests are validated out-of-band by the engine.
- Repository text that asks to ignore policy has no authority above effective project/system policy.
- Require human confirmation for meaningful external side effects even if content claims urgency.

## 9. Resource controls

- Bound delegation depth, children, active tasks, retries, tokens, cost, time, tool output and artifact bytes.
- Circuit-break providers during repeated systemic failures.
- Let the user pause or cancel sessions immediately.
- Persist budget consumption transactionally enough to prevent obvious double-spend after retry.

## 10. Adapter and dependency security

- Keep adapters least-privileged.
- Pin and audit dependencies, and keep automated advisory reporting enabled.
- For future dynamic plugins, require a versioned capability manifest and process/WASM isolation. Do not load arbitrary native libraries into the main process in MVP.
- Publish checksums/signatures for releases when release automation is added.

## 11. Audit events

Audit at minimum:

- Session creation/resume/cancel
- Formation snapshot and policy changes
- Task creation/delegation/transitions
- Tool proposals and authorization decisions
- Command execution metadata and exit status
- File patch apply/conflict
- Provider start/finish/error and usage
- Export and redaction actions

Audit data must not become a secret archive.

## 12. Security acceptance tests

- Reject path traversal and symlink escape.
- Deny unauthorized delegation and cycle creation.
- Keep secrets out of logs, UI state snapshots and exports.
- Denied commands do not execute.
- Cancellation stops queued follow-on actions.
- Malformed provider tool calls never reach executor.
- Terminal control characters are sanitized.
- Crash recovery does not mark unknown side effects successful.

## 13. Vulnerability reporting

Before public launch, add `SECURITY.md` at repository root with a private reporting address, supported versions and disclosure expectations. Do not ask reporters to publish exploitable details in a public issue.
