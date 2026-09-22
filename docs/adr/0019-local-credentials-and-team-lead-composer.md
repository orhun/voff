# ADR 0019: Local credentials and Team Lead composer

## Status

Accepted as a temporary Phase 5 implementation.

## Context

The platform-native secret service used by ADR 0018 is not reliably available
in the target terminal environment. Requiring it prevents otherwise valid
workspaces from using OpenAI. The workspace also needs one obvious place to
inspect current tasks and give new work to the formation's entry Team Lead.

## Decision

Provider credentials entered interactively are stored in a `credentials` table
inside the workspace SQLite database. Process and `.env` values retain higher
precedence. The TOML configuration stores only the lookup reference. On Unix,
voff sets the database file mode to `0600`; diagnostic exports exclude the
credential table and resolved values remain covered by redaction.

This is intentionally simple and is not encrypted at rest. The UI and
documentation label `.voff/voff.db` as sensitive. A future migration may move
credentials to a reliable encrypted store without changing provider or core
domain types.

The main workspace adds a `ratatui-textarea` composer. Plain text creates a
persisted root task owned by the configured entry agent and runs it as a
tool-free conversation, allowing a direct Team Lead response without forcing
a delegation call. `/task <objective>` creates executable root work through the
normal engine, retaining the Team Lead's delegation policy. `/tasks` focuses
the in-memory current-session task list without provider I/O. The composer is
handled on the responsive TUI event loop, while provider and SQLite work
remains asynchronous. Completion or failure is returned to the composer rather
than being silently discarded by a detached task. Enter focuses an inactive
composer, while `/` has no global key binding and remains ordinary input for
slash commands. The border title remains `Team Lead`. Once focused, the empty
input hints at the available commands. Shift+Enter adds lines and dynamically
grows the soft-wrapped editor up to a bounded terminal-relative height; longer
drafts scroll. The TUI enables terminal keyboard enhancement flags so modified
Enter keys remain distinguishable, with Ctrl+J and Alt+Enter as portable newline
fallbacks. The editor exposes a high-contrast block cursor, selection, word-wise
editing, and undo/redo controls. The resolved working directory is right-aligned
beside `? help` on the bottom border. Esc from the active composer performs the
same safe quit as Esc from the surrounding workspace.

Capital `R` remains a workspace action rather than textarea input while the
composer is inactive. It creates a persisted Quick Run root task for the Team
Lead using the standard engine workflow; when the composer is active, `R` is
ordinary text.

The first-launch defaults page contains developer count, an existing working
directory (default `.`), and the shared maximum output-token limit. Provider
and model are selected independently on each developer page. The Team Lead
composer border keeps the resolved working directory visible during the run.
The workspace field launches a directory-only `ratatui-explorer` picker rather
than accepting an unvalidated free-form path.

## Alternatives considered

- Keep the native keyring and make launch depend on a desktop secret service:
  rejected because it is not available in the target environment.
- Store the token in TOML: rejected because workspace configuration should
  remain safe to inspect and commit.
- Add a separate chat protocol: deferred; explicit root tasks already provide
  persistence, ownership, status, cancellation and evidence.

## Consequences

- Interactive credentials work without desktop services or a restart.
- A copied workspace database can expose the token; this limitation must stay
  visible until encrypted storage replaces it.
- Team Lead conversations and executable assignments appear in the same task
  and event history as other work.
- Ratatui and Crossterm move to their 0.30 and 0.29 compatible releases so the
  maintained `ratatui-textarea` crate can be used directly.
