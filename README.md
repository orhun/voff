# voff

**AI agent orchestration, done right.**

voff is a provider-neutral, terminal-first AI agent workspace written in
Rust. It runs local and cloud models as a supervised software team with
persisted tasks, explicit delegation, approvals, dependency-aware scheduling,
and a keyboard-driven Ratatui interface.

## Requirements

- macOS or Linux
- Rust 1.91 or newer
- Ollama when using its local model route
- An API key only for the selected OpenAI cloud route
- An installed, logged-in `codex` or compatible `claude` binary for those local
  CLI routes; they never require an API key

## Quick start: build your team

```bash
git clone https://github.com/voffsh/voff.git
cd voff
ollama pull qwen3:4b
cargo run -p voff
cargo run -p voff -- doctor
```

Voff starts without an implicit task. Press Enter and describe the work to
create an explicit task for the Team Lead, or press capital `R` for Quick Run.
Nothing runs merely because the workspace opened or resumed.

On first launch, or when a bare launch points at a workspace without its local
`.voff/voff.db`, voff opens a quick team builder instead of silently creating
an unrecognized workspace. With `--workspace`, setup stores that workspace's
configuration at `<DIR>/.voff/config.toml` and preselects `<DIR>` as the working
directory. One defaults page sets the developer count, working directory (`.`
by default), and default output-token limit. Names and roles are
suggested automatically; provider and model are selected only on each
developer. The first developer is always the required Team Lead, and later
developers can select specialist roles but cannot create a competing lead.
Press Enter to accept a developer or adjust the available role, provider, or
model choices. OpenAI models come from a
bundled models.dev catalog with context and price metadata; installed Ollama
models are discovered locally when Ollama is running. Codex CLI and Claude CLI
are also selectable; their model field is editable.

The workspace field opens a directory-only `ratatui-explorer` picker. Navigate
with the arrow keys or `hjkl`, press `/` and type to filter directory names,
and press Enter to choose the highlighted folder. Right/`l` opens the
highlighted directory; Left/`h` returns to its parent, and Space chooses the
currently displayed directory. Press Esc to finish or clear a filter, then
again to return without changing the workspace.

Each OpenAI role shows its own optional masked token field on the provider/model
page. Different roles can use different tokens. Local providers do not ask for
one. Leave it blank to keep existing access; use `t` or `c` to change it later.
Tokens are stored in the workspace-local
`.voff/voff.db`, never in `.voff/config.toml`, logs, prompts, or exports.

The workspace includes a `ratatui-textarea` composer at the bottom, with the
resolved working directory pinned to the bottom-right border. Press Enter or
click the composer to start a Team Lead task. Enter sends once the
composer is active, Shift+Enter inserts a newline and grows the composer, and
Esc safely quits. Voff enables modifier-aware terminal input for Shift+Enter;
Ctrl+J or Alt+Enter provides a fallback on terminals without that protocol.
The editor soft-wraps long text and supports selection, word-wise navigation
and deletion, scrolling, and Ctrl+Z/Ctrl+Y undo/redo.
Every submission is executable work that the lead may complete or delegate.
There is no slash-command mode: text beginning with `/` remains part of the
task objective. Every submission becomes an explicit, persisted root task, so
its status and result remain inspectable.

Capital `R` is the explicit Quick Run shortcut. It sends a predefined root task
to the Team Lead to reconcile relevant Markdown requirements and TODOs with the
current source, create a prioritized bounded plan, delegate independent work to
enabled specialists, execute it, validate it, and integrate the evidence. Quick
Run switches directly to Tasks and remains subject to normal approval prompts.
In Tasks, lowercase `r` re-runs the selected completed, failed, cancelled, or
blocked task as a new persisted attempt. The prior attempt remains unchanged
and inspectable.

Risky requests appear in one centered approval popup. When several requests are
pending, use Up/Down to inspect them one at a time; `y` approves only the visible
request once and `n` denies only that request. The action (for example,
`file.patch`) is highlighted in yellow, while the approve/deny keys stay pinned
to the popup's bottom border so long previews cannot hide them.

The workspace uses `rataflow` to render every developer as a live status card.
Every root-work and delegation decision is enforced through the Team Lead;
specialists cannot dispatch work directly to one another. Every specialist has
one relationship connector to the Lead, representing assignments out and
requests/results back through the same coordination hub. Cross-role
collaboration is therefore specialist → Lead → specialist, never a direct peer
edge. Once a persisted child task crosses a route, its connector becomes bright
and animates while work is running. Connectors
intentionally carry no text so parallel routes
remain scannable; the task objective and status stay inside the owning developer
card. Running roles use a pulsing border, activity glyph, and `LIVE` status, so
ownership becomes visible as explicit tasks run. Each card shows its role once inside the border, paired
with the role icon; a distinct custom agent name appears alongside it without
repeating the role as a title. Cards can be selected or repositioned with the mouse;
clicking a role highlights its Team Lead-mediated connector in yellow. Drag
empty graph space to pan, use `+`/`-` to zoom, `0` to reset zoom, and `f`
to fit the team. Press `t` to reopen the same team builder used at startup,
prefilled with the current team. The first page shows **Team settings** and
`[c] Continue`. Change the team size, then review each role's provider, model,
and cloud token. Press `c` on a selected card to jump directly to that role in
the same builder. Finish or cancel pending work before editing. Saving keeps
the current session and persists the team for subsequent launches.
Cards show a short excerpt of the latest provider response and label a
CLI-managed model as `CLI automatic`. New specialists have no peer-delegation
authority. The same status information remains available through text
and symbols, so keyboard-only use and terminals without color remain supported.
Token usage is costed per task owner and model from the same catalog. Explicit
`[cost]` rates override catalog prices; unknown local prices stay visibly
unconfigured rather than being treated as free.

The Rataflow team view owns the full workspace center by default. Press `1` for
Team or `2` for the task workspace; `Tab` toggles between them while the header
and Team Lead composer remain visible. Tasks use a two-pane browser and
inspector: `↑`/`↓` or `j`/`k` selects work, `s` shows its structured summary,
and `a` shows task-scoped activity for the owning role. `PgUp`/`PgDn` scrolls long
results and role activity. Press `e` for the session-wide event viewer without
adding another permanent workspace tab. Tasks are ordered newest first. Event
rows use `◆` for root tasks and indented `└─` connectors for delegated work so
each task's activity remains visually connected to its hierarchy.

Existing configurations skip the builder and open normally; there is no TOML
migration. `voff` reopens the newest task-bearing session when one exists. A
launch recognizes a workspace only when it has usable
configuration and its local `.voff/voff.db`; otherwise it opens setup. Use
`--workspace` to select another project's local configuration and state:

```bash
cargo run -p voff -- --workspace /path/to/project
```

Starting clean is the one explicit launch override:

```bash
cargo run -p voff -- --workspace /path/to/project --new-session
```

Export the newest session as a redacted diagnostic bundle:

```bash
cargo run -p voff -- export \
  --output .voff/diagnostic.json
```

The exporter removes configured credential values and common API-key, token,
password, and bearer-token forms. It refuses to overwrite an existing file so
that an earlier diagnostic is not destroyed accidentally. Preview the JSON
before sharing it because repository/model content is untrusted input.

## Cloud and local providers

Interactive setup stores per-role OpenAI tokens in the local `.voff/voff.db`,
with only credential lookup names in configuration. For shared credentials
in CI or scripts, create an
optional `.env` in the directory
where you invoke voff (no parent directories are searched):

```dotenv
OPENAI_API_KEY=YOUR_API_KEY
```

Existing process variables override `.env`, which overrides the local database.
OpenAI defaults to `OPENAI_API_KEY` when no role token is entered. A separately
entered token uses its own lookup name and is not overridden by the shared
`OPENAI_API_KEY`; an environment value for its exact `api_key_env` name still
takes precedence. Ollama, Codex CLI, and Claude CLI need no token. The CLI routes use the
installed binary's existing login and deliberately do not inherit API-key
variables. Never commit `.env`; it is ignored by Git.
Malformed or unreadable files stop startup with a redacted error. Existing TOML
configurations need no migration.

To use an example formation, place it at the selected workspace's
`.voff/config.toml`, then validate and launch that workspace:

```bash
cargo run -p voff -- doctor
cargo run -p voff
```

Once the workspace opens, press Enter and describe the work you want the
configured formation to perform.

Delegations, patches, and test commands remain subject to explicit approval.
Drag a developer card to arrange the team, click a card to select it, press `y`
to approve once, `n` to deny, `t` to manage specialists, and `c` to configure
the selected role's provider and model for its next task. Cards show the latest bounded
provider-response excerpt alongside current work. Use `1`/`2` to switch
workspace views, `s`/`a` to switch task summary and owner-role activity, `x` to
cancel the selected active task, `r` to re-run a terminal task, `R` for Quick
Run, `f` to fit the team graph, `e` to inspect session events, `?` for help,
and `q` to quit. All core actions remain keyboard-accessible.

## Development checks

Application diagnostics are appended to the selected workspace's
`.voff/logs/voff.log` using `tracing` and `tracing_subscriber`, without writing
over the TUI. They include task IDs, state transitions, request attempts and
approval decisions, but not prompts, response bodies or credentials. Set
`RUST_LOG=debug` for more detail; third-party SDK traces are excluded even at
trace level. On launch, logs of at least 10 MiB rotate to `voff.previous.log`.
There is no configuration migration required.

OpenAI, Ollama, Codex and compatible Claude CLIs stream text or structured-output
previews as they are generated. Previews are display-only: a successful terminal response
and full validation are required before any executable proposal is published.
Interrupted output stays visible with the failed/cancelled task status; it is
not treated as successful work. Transient failures retry up to three attempts
with backoff; a retry resets the preview and never releases the failed attempt's
actions. Codex uses its app-server protocol; Claude requires a recent CLI with
restricted mode and streaming structured-output support. Both use existing CLI
login without an API key. See [provider limitations](docs/PROVIDER_ADAPTERS.md).

Output-token exhaustion pauses for `[y] Continue` / `[n] Cancel` instead of
immediately failing the task. Continue regenerates with a higher task-local
allowance and may cost more; partial tool calls are never applied. New/omitted
defaults are 16,384 tokens; explicit existing limits are preserved. In Tasks and
Events, `[v]` expands/collapses long output, and Summary shows the latest preview
first. Stream persistence is batched to reduce application-side overhead.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

For a real two-role Codex delegation/patch/persistence smoke test (requires
installed Codex login and consumes account usage; no API key required):

```bash
cargo test -p voff-engine --test local_codex -- --ignored --nocapture
```

See [CONTRIBUTING.md](CONTRIBUTING.md), [architecture](docs/ARCHITECTURE.md),
[security model](docs/SECURITY.md), and [roadmap](docs/ROADMAP.md) for project
rules and current milestone evidence.

Security vulnerabilities must follow the private process in
[SECURITY.md](SECURITY.md). Community participation is governed by
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

Maintainers can find the tagged binary workflow and checksum procedure in
[docs/RELEASING.md](docs/RELEASING.md).
Provider contributors should start with
[docs/PROVIDER_ADAPTERS.md](docs/PROVIDER_ADAPTERS.md).

## Security

Never commit API keys or place them in `.voff/*.toml`. Provider credentials
are resolved from environment variables or the workspace-local SQLite database
at runtime. The database stores the token as plaintext with workspace-local
file permissions; treat `.voff/voff.db` as sensitive and do not share it.
voff does not grant
models unrestricted shell or filesystem access.

## License

voff is dual-licensed under either of the following, at your option:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

Unless explicitly stated otherwise, contributions are submitted under the same
dual-license terms.
