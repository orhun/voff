# ADR 0036: Shared team builder and role-scoped cloud credentials

## Context

Startup setup and runtime team/model editors had different controls and
capabilities. Startup collected a single OpenAI token after all roles, so users
could not supply independent cloud credentials alongside each role's provider.

## Decision

Use the onboarding wizard's state, input handling, and renderer for startup,
`t` team editing, and `c` selected-role editing. Runtime editing starts with
the current profiles, fixes the workspace, and requires idle work before entry.
The first page provides `[c] Continue`; edit-mode copy describes the current
team rather than startup defaults.

Cloud token entry is a masked field on each OpenAI role page. Local and CLI
providers have no token field. Secret drafts have redacted Debug output and
never enter serialized configuration. Saving a replacement creates a fresh
opaque lookup reference and stores the token in the existing credential table.
Blank input retains an existing reference or the shared environment fallback.
Each role gets a distinct provider route, even when its provider kind matches
another role. No provider transport changes are needed.

On confirmation the application backs up and saves configuration, rebuilds the
runtime adapters, and reopens the exact same session. Unchanged prompts,
workspace/runtime settings, endpoints, and limits are retained. Unsupported
custom profiles fail closed rather than being converted to a preset. Historical
tasks remain stored; removing a role does not delete its history.

## Consequences

There is one team-editing UI instead of separate runtime editors. Changes
survive restart. Updating credentials does not mutate another role's shared
reference, and a failed configuration save cannot overwrite an old token.
The existing plaintext-at-rest credential-storage limitation remains; this
change does not introduce a keychain or claim encryption.

No schema migration is necessary. Editing reloads the idle runtime rather than
mutating provider routing during active requests. Guided setup remains limited
to supported provider kinds and role presets.
