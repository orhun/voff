# AgentMux

**AI agent orchestration, done right.**

AgentMux is a provider-neutral, terminal-first AI agent workspace written in
Rust. It runs local and cloud models as a supervised software team with
persisted tasks, explicit delegation, approvals, dependency-aware scheduling,
and a keyboard-driven Ratatui interface.

## Requirements

- macOS or Linux
- Rust 1.91.1 (installed automatically by `rustup` from `rust-toolchain.toml`)
- Ollama for local models
- An OpenAI API key only when using the OpenAI provider

## Quick start: local agent

```bash
git clone <repository-url> agentmux
cd agentmux
ollama pull qwen3:4b
cargo run -p agentmux-cli -- init
cargo run -p agentmux-cli -- doctor
cargo run -p agentmux-cli -- run --objective "Inspect this repository and suggest the next safe improvement"
```

`agentmux init` asks for a `solo` or `team` template, `ollama` or `openai`, and
a model, then creates
`.agentmux/config.toml`. It never asks for or stores an API key. For scripts and
repeatable setup, use `--provider`, `--model`, and `--output`:

```bash
cargo run -p agentmux-cli -- init \
  --template team \
  --provider ollama \
  --model qwen3:4b \
  --output .agentmux/config.toml
```

Each `run` creates a clean session. Add `--resume` to reopen the newest stored
session. Previous task histories remain available without being mixed into a
new run.

List stored sessions and resume a specific one:

```bash
cargo run -p agentmux-cli -- sessions
cargo run -p agentmux-cli -- run --session ses_...
```

Export the newest session as a redacted diagnostic bundle:

```bash
cargo run -p agentmux-cli -- export \
  --config .agentmux/heterogeneous.example.toml \
  --output .agentmux/diagnostic.json
```

The exporter removes configured credential values and common API-key, token,
password, and bearer-token forms. It refuses to overwrite an existing file so
that an earlier diagnostic is not destroyed accidentally. Preview the JSON
before sharing it because repository/model content is untrusted input.

## Heterogeneous OpenAI + Ollama team

Keep credentials outside TOML files. On macOS, store the API key in Keychain:

```bash
security add-generic-password -U -a "$USER" -s "agentmux-openai" -w "YOUR_API_KEY"
export OPENAI_API_KEY="$(security find-generic-password -a "$USER" -s "agentmux-openai" -w)"
```

Then validate and run the example formation:

```bash
cargo run -p agentmux-cli -- doctor --config .agentmux/heterogeneous.example.toml
cargo run -p agentmux-cli -- run \
  --config .agentmux/heterogeneous.example.toml \
  --objective "Analyze the project; delegate implementation and review to suitable agents"
```

Delegations, patches, and test commands remain subject to explicit approval.
Press `y` to approve once, `n` to deny, `e` to inspect events, `?` for help,
and `q` to quit.

## Development checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
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

Never commit API keys or place them in `.agentmux/*.toml`. Provider credentials
are resolved from environment variables at runtime. AgentMux does not grant
models unrestricted shell or filesystem access.

## License

AgentMux is dual-licensed under either of the following, at your option:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

Unless explicitly stated otherwise, contributions are submitted under the same
dual-license terms.
