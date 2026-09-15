# Configuration Specification

## 1. Files

- User config: platform config directory, e.g. `~/.config/voff/config.toml`
- Workspace config: `.voff/config.toml`
- Formation files: `.voff/formations/*.toml`
- Prompt files: `.voff/prompts/*.md`

Workspace config should be safe to commit. Secrets must not appear in it.

## 2. Example

```toml
config_version = 1

[workspace]
root = "."
write_mode = "patch_review"
artifact_dir = ".voff/artifacts"

[runtime]
max_parallel_tasks = 3
default_timeout_seconds = 1800
session_cost_limit_usd = 10.0

[providers.openai]
kind = "openai"
credential_env = "OPENAI_API_KEY"
default_model = "configured-by-user"

[providers.anthropic]
kind = "anthropic"
credential_env = "ANTHROPIC_API_KEY"
default_model = "configured-by-user"

[providers.google]
kind = "google"
credential_env = "GOOGLE_API_KEY"
default_model = "configured-by-user"

[[agents]]
id = "lead"
name = "Team Lead"
role = "technical_lead"
provider = "openai"
model = "configured-by-user"
prompt_file = ".voff/prompts/team-lead.md"
tools = ["file.read", "search.text", "task.delegate", "review.request"]

[agents.delegation]
mode = "propose_only"
allowed_targets = ["backend", "frontend"]
max_depth = 2
max_children_per_task = 5

[[agents]]
id = "backend"
name = "Backend Developer"
role = "backend_developer"
provider = "anthropic"
model = "configured-by-user"
prompt_file = ".voff/prompts/backend.md"
tools = ["file.read", "search.text", "file.patch", "process.test"]

[[agents]]
id = "frontend"
name = "Frontend Developer"
role = "frontend_developer"
provider = "google"
model = "configured-by-user"
prompt_file = ".voff/prompts/frontend.md"
tools = ["file.read", "search.text", "file.patch", "process.test"]

[formation]
name = "Lead and Specialists"
entry_agent = "lead"

[permissions]
network_default = "deny"
shell_default = "approval"
write_default = "approval"
allow_paths = ["."]
deny_paths = [".git", ".env", "**/secrets/**"]
```

Provider and model strings are user configuration, not promises that a particular model exists. The UI should discover or validate them at runtime.

## 3. Agent prompt contract

Each role prompt should contain:

- Mission and scope
- Expected inputs and outputs
- Definition of done
- Allowed delegation behavior
- Tool-use rules
- Quality checks
- Escalation conditions
- Explicit instruction to follow repository rules

Example Team Lead prompt:

```markdown
# Role: Technical Team Lead

Turn the user's objective into a bounded, testable plan. Delegate only cohesive
tasks to authorized agents. Give every task acceptance criteria and relevant
context references. Do not implement specialist work merely to keep agents busy.
Inspect child evidence, request review when required, and integrate only
non-conflicting verified results. Ask the user when product intent, security or
irreversible action is ambiguous.
```

## 4. Validation

Fail before session start when:

- `config_version` is unsupported.
- Agent IDs are duplicated or malformed.
- Entry agent does not exist or is disabled.
- Provider reference is missing.
- Delegation target does not exist.
- A prompt path escapes workspace policy.
- Limits are negative, zero where invalid or exceed administrator caps.
- Permission combinations contradict mandatory security policy.

Warnings may cover unavailable optional capabilities, unknown price data and unverified model context limits.

## 5. Precedence and snapshots

Merge configuration using documented precedence, validate once, then persist a redacted immutable formation snapshot with the session. Later config edits do not silently change an active session.

## 6. Secret handling

- Config stores only environment variable names or secret-store references.
- The UI displays whether a credential is available, never its value.
- Export removes resolved secrets and sensitive transient headers.
- Child processes receive only explicitly required variables.

## 7. Formation examples

- `solo`: one agent, delegation disabled.
- `lead-specialists`: lead proposes tasks for specialist agents.
- `review-gated`: implementer results require a separate reviewer.
- `custom`: arbitrary directed delegation relationships.

Formations are graphs, not fixed role enums.
