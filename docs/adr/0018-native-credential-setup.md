# ADR 0018: Native credential setup

## Status

Superseded by ADR 0019 because the native secret service was unreliable in the
target terminal environment.

## Context

Requiring users to leave first-launch setup, export an API token, and restart
undercuts voff's minimal terminal-first onboarding. Accepting a token in TOML,
SQLite, command-line arguments, logs, or normal prompts would expose a secret
and violate the project's security boundary.

## Decision

Interactive first launch offers an OpenAI API token only when at least one
configured developer uses OpenAI and no token already resolves. The field is
optional, supports terminal paste, and renders bullets only. Enter skips it
and launches the workspace. The running TUI shows the missing state and lets
the user press `a` to add the token later without restarting. Keyring writes
run outside the render loop. voff stores the token with the cross-platform
`keyring` crate under the configured environment-reference name. This maps to
macOS Keychain, Windows Credential Manager, or Linux Secret Service.

Credential resolution uses this precedence: process environment, the optional
invocation-directory `.env`, then the OS keyring. Configuration continues to
contain only the lookup name (`OPENAI_API_KEY` by default). Provider adapters
receive a credential slot whose value can become available after composition;
core domain types, persisted
agents, events, diagnostics, prompts, and logs never receive it. Keyring work
is performed outside the terminal render loop.

Non-interactive `voff init`, CI, and headless systems continue to use
environment variables. If a native credential store is unavailable, voff
continues running without the credential and reports the storage error instead
of falling back to plaintext storage.

## Consequences

Cloud-provider first launch remains non-blocking without weakening secret
handling, and a credential added after launch is immediately visible to the
provider adapter. Existing environment-based setups remain compatible and
retain explicit precedence. The binary gains platform keyring dependencies,
and headless Linux environments may need a Secret Service implementation or an
environment variable.
