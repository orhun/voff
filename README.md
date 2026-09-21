# voff

**AI agent orchestration, done right.**

voff is a provider-neutral, terminal-first AI agent workspace written in
Rust. It runs local and cloud models as a supervised software team with
persisted tasks, explicit delegation, approvals, dependency-aware scheduling,
and a keyboard-driven Ratatui interface.

## Requirements

- macOS or Linux
- Rust 1.91 or newer
- Ollama when using local models
- An OpenAI API key only when running OpenAI tasks (not to launch voff)

## Quick start: build your team

```bash
git clone https://github.com/voffsh/voff.git
cd voff
ollama pull qwen3:4b
cargo run -p voff
cargo run -p voff -- doctor
cargo run -p voff -- run --objective "Build a full-stack calculator. Delegate a Python JSON calculation API to Backend Developer and an accessible browser UI that calls it to Frontend Developer. Both workstreams are required."
```

The built-in calculator demo is intentionally full-stack: the Backend
Developer owns a standard-library `server.py` JSON API and the Frontend
Developer owns an accessible `index.html` client. The Team Lead is instructed
to create both persisted child tasks before the objective can complete. Every
other enabled specialist also receives one role-appropriate, non-overlapping
review, validation, integration, or operations task. Disabled specialists are
skipped, and specialists added with `S` are included when `r` starts the run.

On first launch, voff opens a quick team builder. One defaults page sets the
developer count, working directory (`.` by default), and default output-token
limit. Names and roles are
suggested automatically; provider and model are selected only on each
developer. The first developer is always the required Team Lead, and later
developers can select specialist roles but cannot create a competing lead.
Press Enter to accept a developer or adjust the available role, provider, or
model choices. OpenAI models come from a
bundled models.dev catalog with context and price metadata; installed Ollama
models are discovered locally when Ollama is running.

The workspace field opens a directory-only `ratatui-explorer` picker. Navigate
with the arrow keys or `hjkl`, press `/` and type to filter directory names,
and press Enter to choose the highlighted folder. Right/`l` opens the
highlighted directory; Left/`h` returns to its parent, and Space chooses the
currently displayed directory. Press Esc to finish or clear a filter, then
again to return without changing the workspace.

If the selected team uses OpenAI and no credential is configured, the builder
offers an optional masked token field. Press Enter to skip it: a missing API
token never blocks launch. In the workspace, press `a` to add it later without
restarting. Values entered either way are stored in the workspace-local
`.voff/voff.db`, never in `.voff/config.toml`, logs, prompts, or exports.

The workspace includes a `ratatui-textarea` composer at the bottom, with the
resolved working directory always visible in its border. Click the composer or
press Enter, type a message, and press Enter again to send it to the Team Lead.
Plain text asks the Team Lead for a direct, tool-free response. Type
`/task <objective>` to give executable work that the lead may delegate, or
`/tasks` to focus the current task list. Both conversations and assignments
become explicit, persisted root tasks, so their status and results remain
inspectable.

Risky requests appear in one centered approval popup. When several requests are
pending, use Up/Down to inspect them one at a time; `y` approves only the visible
request once and `n` denies only that request.

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
card. Running roles use a pulsing border, activity
glyph, and `LIVE` status, so pressing `r` makes ownership
visible immediately. Cards can be selected or repositioned with the mouse;
clicking a role highlights its Team Lead-mediated connector in yellow. Drag
empty graph space to pan, use `+`/`-` to zoom, `0` to reset zoom, and `f`
to fit the team. Press `S` after setup to open the team popup. `n` adds another
Backend, Frontend, Full-stack, Reviewer, Tester, or DevOps specialist using the
Team Lead's provider/model defaults; Enter enables or disables the selected idle
specialist. The Team Lead cannot be disabled, and a specialist with queued,
running, or pending-approved work must finish or cancel it before availability
changes. New specialists have no peer-delegation authority. The same status information remains available through text
and symbols, so keyboard-only use and terminals without color remain supported.
Token usage is costed per task owner and model from the same catalog. Explicit
`[cost]` rates override catalog prices; unknown local prices stay visibly
unconfigured rather than being treated as free.

The Rataflow team view owns the full workspace center by default. Press `1` for
Team, `2` for Tasks, `3` for Task Details, or `4` for Events; `Tab` cycles the
same fullscreen views while the header and Team Lead composer remain visible.

Existing configurations skip the builder and open normally; there is no config
migration. For scripts and repeatable setup, `voff init` remains available with
`--provider`, `--model`, and `--output`:

```bash
cargo run -p voff -- init \
  --template team \
  --provider ollama \
  --model qwen3:4b \
  --max-output-tokens 8192 \
  --output .voff/config.toml
```

Each `run` creates a clean session. Add `--resume` to reopen the newest stored
session. Previous task histories remain available without being mixed into a
new run.

List stored sessions and resume a specific one:

```bash
cargo run -p voff -- sessions
cargo run -p voff -- run --session ses_...
```

Export the newest session as a redacted diagnostic bundle:

```bash
cargo run -p voff -- export \
  --config .voff/heterogeneous.example.toml \
  --output .voff/diagnostic.json
```

The exporter removes configured credential values and common API-key, token,
password, and bearer-token forms. It refuses to overwrite an existing file so
that an earlier diagnostic is not destroyed accidentally. Preview the JSON
before sharing it because repository/model content is untrusted input.

## Heterogeneous OpenAI + Ollama team

Interactive setup and the workspace's `a` shortcut store the OpenAI API token
in the local `.voff/voff.db`. For CI, scripts, or an explicit override, create an
optional `.env` in the directory
where you invoke voff (no parent directories are searched):

```dotenv
OPENAI_API_KEY=YOUR_API_KEY
```

Existing process variables override `.env`, which overrides the local database.
OpenAI defaults to `OPENAI_API_KEY`; a custom `api_key_env` name works
identically. Ollama needs no token. Never commit `.env`; it is ignored by Git.
Malformed or unreadable files stop startup with a redacted error. Existing TOML
configurations need no migration.

Then validate and run the example formation:

```bash
cargo run -p voff -- doctor --config .voff/heterogeneous.example.toml
cargo run -p voff -- run \
  --config .voff/heterogeneous.example.toml \
  --objective "Build a full-stack calculator and delegate implementation to Local Implementer, then delegate an independent API/UI integration review to Cloud Reviewer. Both workstreams are required."
```

Delegations, patches, and test commands remain subject to explicit approval.
Drag a developer card to arrange the team, click a card to select it, press `y`
to approve once, `n` to deny, `S` to add or adjust specialists, `1`–`4` to
switch fullscreen views, `f` to fit the team graph, `e` to inspect events, `?`
for help, and `q` to quit. All core actions remain keyboard-accessible.

## Development checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
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
