# ADR 0025: Minimal workspace CLI

## Status

Accepted.

## Context

voff is a terminal application, but its command line had grown into a second
product surface. `init` duplicated guided first-launch setup, `run` duplicated
normal launch, `sessions` exposed storage identifiers without a corresponding
workspace experience, and each command repeated a configuration path even
though configuration and state are workspace-local.

The overlap made the common path harder to explain: bare launch resumed while
`voff run` unexpectedly created a clean session, and `--resume`, `--session`,
`--config`, and `--workspace` could describe competing locations or histories.

## Decision

- `voff [--workspace DIR]` is the application. It opens guided setup for an
  unrecognized workspace and otherwise resumes the newest task-bearing session.
- `--new-session` is the only launch-mode override.
- `.voff/config.toml` and `.voff/voff.db` are resolved from the selected
  workspace. Arbitrary command-specific configuration paths are removed.
- Keep only two non-interactive subcommands with distinct operational value:
  `doctor` validates the selected workspace and `export` produces a redacted
  diagnostic bundle.
- Remove `init`, `run`, `sessions`, `--resume`, `--session`, and `--config`.
  Advanced or automated configuration remains possible by writing the
  documented versioned TOML file into `.voff/config.toml`.

## Alternatives

- Keep every command as a compatibility layer. This preserves scripts but
  retains two ways to perform setup and launch, which is the confusion being
  removed before a stable release.
- Remove the CLI entirely. A binary still needs workspace selection, help,
  version reporting, provider diagnosis, and a way to create redacted support
  bundles.
- Move diagnostics into the TUI immediately. This is desirable later, but does
  not justify coupling diagnostic output to a functioning renderer today.

## Consequences

The primary journey is one command and one workspace. New-session creation is
explicit, while normal restart recovery requires no flag. Existing invocations
must migrate as follows:

- `voff run --resume` becomes `voff`.
- `voff run` becomes `voff --new-session`.
- `voff ... --config PATH` requires placing that configuration at the selected
  workspace's `.voff/config.toml`.
- `voff init` becomes normal interactive first launch.

Exact selection of older session IDs is no longer a public CLI workflow. The
database schema and historical records are unchanged; a future session browser
belongs in the TUI rather than a storage-oriented command.
