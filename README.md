# voff

voff is a terminal workflow builder for coding agents. On launch, it can inspect a workspace and create a starter team, or continue a saved team. Tasks, delegation, approvals, and activity are visible and saved locally.

## Install

Requires Rust 1.92+ and a supported provider: an installed Codex or Claude CLI, Ollama, or an OpenAI API token.

```sh
cargo install --path crates/voff
voff
voff ~/gh/my-project
voff --config .voff/config.toml
```

Run `voff` in a workspace, pass its directory as the first argument, or load a team from TOML with `--config`. Voff automatically loads `.voff/config.toml` when present. The setup page lets you choose a client and model, then either press Enter to build a team from the workspace or press `d` and describe what you want to build. Voff generates the team lead prompt and the project-specific agents from that context. OpenAI cloud is selectable without an environment key; select it and press `c` to enter a key. The generated team is saved in `.voff/config.toml`, while runtime state and credentials stay in `.voff/voff.db`.

The TOML format is intentionally small. Each `[[agent]]` entry supports `name`, `provider`, `model`, `prompt`, and `reports_to` (the name of another agent). Voff uses a sensible default output-token limit. The first agent is the team lead. Relative `workspace` paths resolve from the TOML file; a positional workspace argument overrides it. The generated `[setup_prompts]` section contains the two built-in team-generation prompts and can be edited to customize how Voff plans a team.

```toml
workspace = "."

[[agent]]
name = "Lead"
provider = "codex_cli"
model = "default"
prompt = "Coordinate the work, delegate bounded tasks, and report verified results."

[[agent]]
name = "Agent 2"
provider = "codex_cli"
model = "default"
prompt = "Take bounded tasks from the team lead, follow repository conventions, and report changes and checks."
reports_to = "Lead"
```

Cloud credentials are read from environment variables and are never stored in the TOML file. In the app, press `w` or click the workspace path to choose another workspace; voff then reopens that workspace and its database.

In the Team map, press `a` to add a generic agent. Agent names, reporting, provider, model, token settings, and prompts can be adjusted in agent settings. Press `e` to edit the selected agent's prompt (`Ctrl+E` works while typing in a text field); Ctrl+S applies the prompt, then Enter saves the team to `.voff/config.toml` and shows a notification. A new agent reports to the selected agent by default. Press Delete to remove an agent after confirmation; the team lead and agents with direct reports cannot be removed. Use Tab or the arrow keys to move between fields.

In the app, press Enter to write a task. The Team and Tasks tabs are always available: press `1` or `2` to select one, or use Tab and Shift+Tab to cycle through tabs. In the Team view, press `t` to switch focus between the task pane and team map; press `h` to show or hide the task pane. Click an agent card to select it, then press `o`, double-click it, or use the `+` tab control to open its tab. Press `c` or Ctrl+W to close the active agent tab. Delegation follows each agent's direct reports, and results return to the reporting agent for review. Click or drag cards to rearrange the team view, or select an agent and press Ctrl+arrow keys to nudge its card.

Press `u` to inspect token usage, task progress, and per-agent cost estimates. Cost estimates use the bundled model catalog and may be unavailable for local CLI providers or models without catalog pricing.
