# Configuration Specification

## 1. Files

- User config: platform config directory, e.g. `~/.config/voff/config.toml`
- Workspace config: `.voff/config.toml`
- Formation files: `.voff/formations/*.toml`
- Prompt files: `.voff/prompts/*.md`

Workspace config should be safe to commit. Secrets must not appear in it.

On first interactive launch, when `.voff/config.toml` does not exist, the
terminal team builder asks for the developer count, an existing working
directory (`.` by default), and one default output-token limit. It then offers
an automatic name and role plus a
provider/model selection for each developer, so accepting the suggested team
requires only Enter. The first developer is a required Technical Lead and
cannot be changed to a specialist; later developers cannot select the lead
role. Generated delegation policy is a mediated hub topology: only the lead
receives delegation tools and allowed targets, so specialists return requests
and results to the lead instead of dispatching directly to peers. The graph
shows one connector per Lead/specialist relationship, making cross-role work
visible through the shared Lead hub without granting peer authority. A
one-developer setup still uses the required Team Lead, but gives it scoped
patch/test tools instead of delegation tools so
the solo team remains useful. OpenAI
choices are filtered to text/tool-capable offerings from the bundled
models.dev snapshot. Ollama choices come from the models installed on the
loopback runtime when it responds within the bounded discovery window; an
editable fallback remains available when it does not. It
writes the same version 1 schema documented below. Existing configurations are
loaded unchanged and default to `4096` output tokens, so this onboarding flow
requires no migration. Non-interactive environments should use `voff init`
with explicit flags.

The selected working directory is written to `workspace.root`. It scopes
workspace file access, process execution, and the local `.voff/voff.db`; the
main Team Lead bar shows its resolved path while voff is running.
The global `--workspace <DIR>` argument temporarily overrides this value for
the current invocation and uses `<DIR>/.voff/voff.db` (or the configured
relative `state_file`). It is accepted before or after a subcommand, applies to
`run`, `sessions`, `doctor`, and `export`. For an already recognized workspace,
the override does not rewrite configuration. On a bare launch, a missing
workspace database marks the target as unrecognized and opens interactive
setup rather than silently initializing state. A new `--workspace` target
receives its own `.voff/config.toml`; if setup replaces an existing
configuration because its database is missing, the prior file is preserved
beside it as `config.toml.bak` (with a numeric suffix when needed). Explicit
subcommands retain their non-interactive behavior.
Interactive setup selects it through a directory-only `ratatui-explorer`
picker. Arrow keys and `hjkl` navigate, `/` starts a typed directory-name
filter, and Enter accepts the highlighted directory. Right/`l` opens the
highlighted directory, Left/`h` returns to its parent, and Space accepts the
currently displayed directory. Esc first finishes or clears an active filter
and then preserves the previous workspace selection.

After launch, `S` opens a modal team adjustment surface. It changes an idle
specialist's enabled state for future assignments in the active runtime, and
`n` adds another specialist from the safe Backend, Frontend, Full-stack,
Reviewer, Tester, or DevOps presets. Added specialists inherit the Team Lead's
provider, model, and token limit, receive role-appropriate tools, and have no
delegation targets. The required Team Lead cannot be disabled, and active or
approval-pending work blocks availability changes. Accepted changes update
persisted agent profiles and append audit events. Runtime additions apply to the
active run; durable membership/provider/model customization still requires a
validated configuration update and a new run.

Launching or resuming a workspace creates no implicit task. Submit explicit work
with `/task <objective>`, or press capital `R` for Quick Run. The Team Lead may
then propose bounded delegation to enabled, allowed specialists through the
normal approval flow.

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
max_output_tokens = 8192
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
max_output_tokens = 4096
prompt_file = ".voff/prompts/backend.md"
tools = ["file.read", "search.text", "file.patch", "process.test"]

[[agents]]
id = "frontend"
name = "Frontend Developer"
role = "frontend_developer"
provider = "google"
model = "configured-by-user"
max_output_tokens = 4096
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

The application uses `ai-model-directory-router`'s bundled, dated models.dev
snapshot for provider-qualified model metadata and exact decimal cost
calculation. This is offline metadata, not an entitlement check: the selected
model must still be available to the user's provider account. Runtime usage is
priced against the task owner's provider/model and shown as a USD estimate.
Unknown catalog prices remain unconfigured, and a heterogeneous total is
labelled partial if any usage could not be priced. If both `[cost]`
`input_per_million` and `output_per_million` are set, those user rates override
catalog pricing and `currency` controls the displayed unit.

`max_output_tokens` is a per-response generation cap passed through the
provider-neutral request contract. It is not an API credential or a cumulative
billing budget. Values must be between `1` and `1_000_000`; configurations that
omit it retain the backward-compatible `4096` default.

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

At executable startup, voff loads an optional `.env` from the current working
directory only, before configuration and authentication resolution. Existing
process variables take precedence; a missing file is harmless. Malformed or
unreadable files fail with a safe error. Never commit `.env`.

Interactive setup offers an optional field for the actual OpenAI API token if
the selected team needs it and no credential already exists. Enter skips it,
so credentials never block workspace launch. Press `A` in the running
workspace to enter the token later without restarting. Input is masked and
stored in the workspace-local SQLite database at `.voff/voff.db`. The value is
never serialized to project configuration, prompts, logs, or exports. This is
a deliberately simple temporary mechanism: the token is plaintext at rest,
and the database must be treated as sensitive.

Environment variables have precedence over the local database. OpenAI uses
`OPENAI_API_KEY` by default, or the name configured in `api_key_env`; Ollama
requires no token. Minimal non-interactive placeholder example:

```dotenv
OPENAI_API_KEY=YOUR_API_KEY
```

No TOML migration is required.

- Config stores only environment-variable/database lookup names.
- The UI displays whether a credential is available, never its value, and
  permits a missing OpenAI credential to be added with `A`.
- Export removes resolved secrets and sensitive transient headers.
- Child processes receive only explicitly required variables.

## 7. Team Lead composer

The workspace footer is a `ratatui-textarea` input titled `Team Lead`. Press
Enter or click the composer to start talking to the Team Lead. Once active,
Enter sends, Shift+Enter inserts a newline and grows the composer, and Esc
safely quits. The TUI requests modifier-aware keyboard reporting so Shift+Enter
is distinguishable from Enter; Ctrl+J and Alt+Enter are newline fallbacks for
terminals without that protocol. Long drafts soft-wrap and scroll; arrows,
Home/End, Ctrl+arrows, Shift+arrows, Ctrl+W/Ctrl+K, and Ctrl+Z/Ctrl+Y provide
navigation, selection, deletion, and undo/redo. Plain text
creates a persisted root task owned by the formation entry agent (the Team
Lead) and requests a direct, tool-free answer. `/task <objective>` creates
executable work for the Team Lead, retaining its configured delegation policy.
`/tasks` focuses the current session's live task list without calling a
provider. The resolved working directory stays on the bottom-right border.
Submission failures are reported in the composer instead of being discarded by
a background task.

Capital `R` starts Quick Run only while the composer is inactive. It creates a
persisted Team Lead root task with a predefined prompt to reconcile relevant
Markdown requirements with the source, prioritize bounded tasks, delegate
independent workstreams when specialists are available, execute approved work,
and validate the results. The UI switches to Tasks immediately and marks the
workflow `R QUICK RUN`; all delegation and side-effect approvals still apply.
The inactive composer keeps a centered white `[R] Quick Run` label on its
bottom border; the workspace header is reserved for view and runtime status.

In the Tasks view, lowercase `r` creates and runs a new attempt for the selected
completed, failed, cancelled, or blocked task. The new attempt retains its own
task ID, attempt number, and link to the immediately preceding attempt; the
prior terminal history is never reset or overwritten. Active and
approval-pending tasks must be cancelled or resolved before they can be
re-run.

The task browser puts Team Lead roots first, orders peer tasks newest first, and
keeps every delegated task directly beneath its parent. Each delegated task
card, including its owner and objective, is indented by two columns per
delegation depth; root owners have no hierarchy padding. Each owner uses the
same colored Unicode role icon as the Team view. The session event viewer and
role activity view stay flat and identify events with short task IDs. Event text
uses semantic colors while retaining textual status: actions and approvals are
yellow, failures red, completions green, task lifecycle events cyan, and usage
magenta.

## 8. Formation examples

- `solo`: one agent, delegation disabled.
- `lead-specialists`: lead proposes tasks for specialist agents.
- `review-gated`: implementer results require a separate reviewer.
- `custom`: arbitrary directed delegation relationships.

Formations are graphs, not fixed role enums.
