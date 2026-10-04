# voff

voff is a terminal workspace for coordinating a team of AI agents. Tasks, delegation, approvals, and activity are visible and saved locally.

## Install

Requires Rust 1.92+ and a supported provider: an installed Codex or Claude CLI, Ollama, or an OpenAI API token.

```sh
cargo install --path crates/voff
voff
voff ~/gh/my-project
```

Run `voff` in a workspace, or pass its directory as the first argument. First launch asks only for Team Lead settings; press Enter through the defaults to start. Cloud tokens are requested only for cloud providers. Team choices and tasks live in that workspace's `.voff/voff.db`. In the app, press `w` or click the workspace path to choose another workspace; voff then reopens that workspace and its database.

In the Team map, press `a` to add a Backend, Frontend, Reviewer, or Custom Agent. A new agent reports to the selected agent by default; change **Reports to** in its settings to build a different reporting tree. Press Delete to remove a selected agent after confirmation; the Team Lead and agents with direct reports cannot be removed. Built-in roles have editable prompts; Custom Agent starts with an empty prompt. In agent settings, select Prompt and press `e` to edit it; Ctrl+S saves and Esc discards. Type directly to edit a name, model, or token; use Tab or the arrow keys to move between fields. Enter continues or saves the agent settings.

In the app, press Enter to write a task, `1` for Team, `2` for Tasks, and `c` to configure the selected agent. Click a role card to select it, or double-click or press `t` to open its tab; Ctrl+W closes a role tab. Delegation follows each agent's direct reports, and results return to the reporting agent for review. You can click tabs and task controls, or drag cards to rearrange the team view.
