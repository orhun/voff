# Troubleshooting

Start with `agentmux doctor --config .agentmux/config.toml`.

## OpenAI

- **Credential variable missing:** export the exact variable named by
  `api_key_env`; never put the key in TOML.
- **Authentication or authorization:** replace/revoke the key in its secret
  store and export it into the current shell.
- **Insufficient quota:** API billing is separate from ChatGPT subscriptions.
- **Context length or invalid request:** reduce task context or select a model
  compatible with the requested tools.

## Ollama

- Start the local Ollama application/service and confirm `ollama list` works.
- Only loopback URLs are accepted; remote Ollama endpoints are rejected.
- Confirm the configured model is installed locally.

## Retry and circuit state

Only explicitly retryable pre-stream failures are retried. AgentMux never
retries after emitting text or a tool request. Repeated faults open the circuit
and appear in the Agents panel.

## Diagnostics

Use `agentmux export` with a new output path. Export refuses to overwrite an
existing file. Inspect redacted JSON before sharing because model and repository
content remains untrusted project data.

