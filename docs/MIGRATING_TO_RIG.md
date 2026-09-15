# Migrating AgentMux to the Rig provider backend

AgentMux 0.1.0-beta.1 now uses Rig 0.42.0 internally for OpenAI and Ollama.
There is no migration command and no provider-backend switch.

## Existing installations

- Keep `.agentmux/config.toml` unchanged.
- Keep OpenAI keys in the referenced environment variable or OS keychain.
- Existing SQLite sessions remain readable; Rig state is not stored in them.
- Run `agentmux doctor --config .agentmux/config.toml` before resuming work.

The removed `AGENTMUX_PROVIDER_BACKEND` environment variable and
`rig-provider` Cargo feature are ignored because the alternative legacy stack
no longer exists. Remove them from scripts.

## Verification

```bash
cargo run -p agentmux-cli -- doctor --config .agentmux/config.toml
cargo run -p agentmux-cli -- sessions --config .agentmux/config.toml
cargo run -p agentmux-cli -- run --config .agentmux/config.toml --resume
```

Back up `.agentmux/agentmux.db` before unrelated future schema upgrades. To
roll back this provider migration, install the previous AgentMux release; do
not copy or rewrite the database and do not enable a hidden legacy feature.

