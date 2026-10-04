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
editable fallback remains available when it does not. Codex CLI and Claude CLI
choices use editable provider defaults. It
writes the same version 1 schema documented below. Existing configurations are
loaded unchanged; omitted token limits now default to `16384` output tokens.
Explicit limits such as `4096` are preserved. No TOML schema migration is
required. Automated environments may write the documented
schema directly to `.voff/config.toml`; there is no separate setup command.

The selected working directory is written to `workspace.root`. It scopes
workspace file access, process execution, and the local `.voff/voff.db`; the
main Team Lead bar shows its resolved path while voff is running.
The global `--workspace <DIR>` argument selects `<DIR>/.voff/config.toml` and
uses `<DIR>/.voff/voff.db` (or the configured relative `state_file`). It is
accepted before or after `doctor` and `export`. For an already recognized
workspace, the override does not rewrite configuration. On launch, a missing
workspace database marks the target as unrecognized and opens interactive
setup rather than silently initializing state. A new `--workspace` target
receives its own `.voff/config.toml`; if setup replaces an existing
configuration because its database is missing, the prior file is preserved
beside it as `config.toml.bak` (with a numeric suffix when needed). Explicit
diagnostic subcommands remain non-interactive.
Interactive setup selects it through a directory-only `ratatui-explorer`
picker. Arrow keys and `hjkl` navigate, `/` starts a typed directory-name
filter, and Enter accepts the highlighted directory. Right/`l` opens the
highlighted directory, Left/`h` returns to its parent, and Space accepts the
currently displayed directory. Esc first finishes or clears an active filter
and then preserves the previous workspace selection.

After launch, `t` opens the same team builder used at startup, prefilled with
the current team. Its first page is **Team settings**, with a visible
`[c] Continue` control. Reopening says **Edit your current team**, not quick
defaults. Existing choices are preserved when continuing; changing the count
adds or removes trailing roles. The active workspace stays fixed.

With a role highlighted, `c` opens that role in the same builder. Tab/Up/Down
moves between fields; Left/Right changes choices. Cloud roles show an optional
masked API-token field on their own page; Ollama and CLI roles do not.
The final review saves the team to workspace configuration (backing up the
previous file), reloads provider routes, and returns to the same session.
Pending work must finish or be cancelled first. Esc goes back; Esc on the first
page cancels without saving. Custom roles/providers not representable by guided
setup are refused rather than silently converted to presets.

Launching or resuming a workspace creates no implicit task. Press Enter and
describe explicit work, or press capital `R` for Quick Run. The Team Lead may
then propose bounded delegation to enabled, allowed specialists through the
normal approval flow.
## 2. Example

Routine work now defaults to Team Lead review instead of human Y/N approval.
This also applies to version-1 files that omit the new field:

```toml
[runtime]
automatic_review = true # false restores manual per-action review
default_timeout_seconds = 1800
event_channel_capacity = 256
```

The configured Lead's task assignments (including review assignments) dispatch
directly after runtime validation, without a popup or a second model review.
The Lead reviews patches and allowlisted verification commands using its own
provider/model. Decisions and reasons are persisted in
Activity; the diagram reverses the specialist's edge during review, using the
same connection points (Lead bottom / specialist top) in both directions.
An active return-for-review arrow takes visual priority at the shared Lead port
over outgoing assignment diamonds.
Activity starts with a chronological review-loop overview for the selected task
and its retry chain: latest attempt/status, review, verdict, returned feedback,
new attempt and re-review. It shows the six latest handoffs separately from the
newest-first role log; `v` expands feedback. This is a view of recorded events,
not a promise that every rejection will retry or that approval means completion.
Recoverable
work failures can receive two corrective attempts, preserving their history.
Valid Lead rejections return feedback to the same owner for a revised proposal
and fresh review, within that same three-attempt budget. The rejected action
is never executed; unchanged resubmissions stop the loop. Human/runtime denials
and failed or malformed reviews do not retry. Login problems show an application notice; token
increases still require your decision. This does not authorize arbitrary
commands, sensitive configuration writes, or external publication. Review adds
model latency and cost. Use manual mode for untrusted workspaces: the executor
is not an OS sandbox. See ADR 0032 for restart behavior and remaining limits.

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
omit it use the `16384` default (increased from `4096`). To retain the former
allowance, set `max_output_tokens = 4096` explicitly. Larger limits can permit
longer, more expensive responses; they do not increase model generation speed.

When a provider explicitly reports output-token exhaustion, the task waits for
a persisted **Continue / Cancel** decision. `y` regenerates the incomplete
response with twice the task-local allowance; `n` cancels that task. Partial
proposals are never executed, and all normal tool approvals still apply. This
does not change the role's configured limit or other tasks. Continuation is
bounded to three increases and at most 131072 tokens; if exhausted, reduce the
task scope or change its model before re-running. Provider model limits still
apply. Pending decisions survive restart. Content filtering and malformed
responses are not treated as permission to increase the allowance.

Approval/confirmation requests normally appear as compact top-right notices
with a short caption and border controls `[y] [n] [d] details`. `d` maximizes
the selected request; `d` or Esc minimizes it again, and PgUp/PgDn scroll its
full details. Up/Down selects another pending request. Configuration/help
surfaces remain full editing or inspection dialogs.
In automatic mode, routine work requests appear in the graph/activity instead
of this popup; only application decisions such as token continuation interrupt.
Application issues use a dismissible top-right notice (`Esc` dismisses; `e`
shows full event details).

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

Each OpenAI role has an optional masked API-token field beside its provider
and model, even when another role already has credentials. Blank retains the
existing reference, or uses `OPENAI_API_KEY` for a new role. Entering a token
creates a separate opaque credential reference for that role; other roles'
tokens are never overwritten. Use `t` or `c` to replace it later.
Local providers never show this field. Values are
stored in the workspace-local SQLite database at `.voff/voff.db`. The value is
never serialized to project configuration, prompts, logs, or exports. This is
a deliberately simple temporary mechanism: the token is plaintext at rest,
and the database must be treated as sensitive.

Environment variables have precedence over the local database. OpenAI uses
`OPENAI_API_KEY` by default, or the name configured in `api_key_env`. Ollama
requires no token. Codex CLI and Claude CLI use the installed binary's existing
login and never resolve or inherit an API key. Minimal non-interactive
placeholder example:

```dotenv
OPENAI_API_KEY=YOUR_API_KEY
```

Provider configuration uses one of these version-1 shapes:

```toml
[provider]
kind = "openai"
model = "gpt-5-mini"
api_key_env = "OPENAI_API_KEY"

# Credential-free installed CLI alternative:
# [provider]
# kind = "codex_cli" # or "claude_cli"
# model = "default"
```

`doctor` checks provider connectivity and verifies that a local binary supports
the required structured-output and safety flags. An older CLI fails with an
actionable update message instead of starting an unsafe or unstructured run.

No TOML or SQLite schema migration is required. Saved teams use one named
provider route per role. Old shared references continue to work until a role
receives its own token. Only lookup names appear in configuration, for example:

```toml
[[providers]]
id = "openai-developer-1"
kind = "openai"
model = "gpt-5-mini"
api_key_env = "VOFF_OPENAI_ROLE_AGT_EXAMPLE"
```

The environment override applies to that exact lookup name; `OPENAI_API_KEY`
does not override a separately entered role token.

- Config stores only environment-variable/database lookup names.
- The UI displays whether a credential is available, never its value, and
  permits a missing OpenAI credential to be added with `A`.
- Export removes resolved secrets and sensitive transient headers.
- Child processes receive only explicitly required variables.

## 7. Team Lead composer

The workspace footer is a `ratatui-textarea` input titled `Team Lead`. Press
Enter or click the composer to start a Team Lead task. Once active,
Enter sends, Shift+Enter inserts a newline and grows the composer, and Esc
safely quits. The TUI requests modifier-aware keyboard reporting so Shift+Enter
is distinguishable from Enter; Ctrl+J and Alt+Enter are newline fallbacks for
terminals without that protocol. Long drafts soft-wrap and scroll; arrows,
Home/End, Ctrl+arrows, Shift+arrows, Ctrl+W/Ctrl+K, and Ctrl+Z/Ctrl+Y provide
navigation, selection, deletion, and undo/redo. Every non-empty submission
creates executable, persisted root work owned by the formation entry agent (the
Team Lead), retaining its configured delegation policy. There is no composer
command mode; leading `/` characters remain ordinary task text. The resolved
working directory stays on the bottom-right border. Submission failures are
reported in the composer instead of being discarded by a background task.

Capital `R` starts Quick Run only while the composer is inactive. It creates a
persisted Team Lead root task with a predefined prompt to reconcile relevant
Markdown requirements with the source, prioritize bounded tasks, delegate
independent workstreams when specialists are available, execute approved work,
and validate the results. The UI switches to Tasks immediately and marks the
workflow `R QUICK RUN`; all delegation and side-effect approvals still apply.
The inactive composer keeps a centered white `[R]` label on its
bottom border; the workspace header is reserved for view and runtime status.

In the Tasks view, lowercase `r` creates and runs a new attempt for the selected
completed, failed, cancelled, or blocked task. The new attempt retains its own
task ID, attempt number, and link to the immediately preceding attempt; the
prior terminal history is never reset or overwritten. Active and
approval-pending tasks must be cancelled or resolved before they can be
re-run.

Resume reuses session-owned agent IDs for matching configured names and roles.
Older root attempts from before this fix can be re-run under the current Team
Lead only when old and current names and roles match exactly. Historical task
ownership stays unchanged; specialists are never promoted by this compatibility
handling.

The task browser puts Team Lead roots first, orders peer tasks newest first, and
keeps every delegated task directly beneath its parent. Each delegated task
card, including its owner and objective, is indented by two columns per
delegation depth; root owners have no hierarchy padding. Each owner uses the
same colored Unicode role icon as the Team view. The session event viewer and
role activity view stay flat and identify events with short task IDs. Event text
uses semantic colors while retaining textual status: actions and approvals are
yellow, failures red, completions green, task lifecycle events cyan, and usage
magenta.

Summary places the latest generated output before the task brief. Long output,
acceptance lists, artifact previews and activity entries are compact by default;
`v` expands/collapses them without altering persisted content. Compact live
output shows its newest tail. Summary/Activity and Team/Tasks use a filled active
tab without marker icons, while inactive choices remain readable. Generation
fragments are coalesced into at most one normal persistence update per 50 ms
(also flushing at type changes, bounded chunk size and completion), reducing
per-token database overhead without simulating streaming.

## 8. Formation examples

New TUI tasks use contract-first verified work (ADR 0035). Implementation plans
include a shared interface contract and a separate integration task, which waits
for all implementation results. Every assigned role needs `process.test` for
verification; choose an integration owner capable of correcting the combined
application. Allow room for integration in `max_children`.

An approved patch now means progress, not completion. The owner continues with
additional files/checks and submits criterion-by-criterion evidence for final
acceptance. The Summary displays the shared contract, integration stage and
turn budget. Verified work is currently serialized in the shared checkout and
bounded to 32 turns across retries. Existing stored tasks retain their original
semantics; submit a new objective to use this workflow. There is no config-file
migration or automatic rewrite of old completed work.

`process.test` accepts exact Cargo verification commands plus `python3 -m unittest
discover`, `node --test`, `npm test`, and `npm run build`, all at the workspace
root. A successful test suite after the last workspace patch is required for
completion; build/check alone do not qualify. Dependencies must already be
installed. Package installation and arbitrary shell commands remain unavailable.
These runners execute project code and are not an OS/network sandbox.

- `solo`: one agent, delegation disabled.
- `lead-specialists`: lead proposes tasks for specialist agents.
- `review-gated`: implementer results require a separate reviewer.
- `custom`: arbitrary directed delegation relationships.

Formations are graphs, not fixed role enums.
