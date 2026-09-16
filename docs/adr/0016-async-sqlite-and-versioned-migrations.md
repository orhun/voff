# ADR 0016: Async SQLite and versioned migrations

## Context

Repository operations are asynchronous, but direct `rusqlite` calls execute blocking database work on the Tokio runtime. The original migration implementation also duplicated version bookkeeping in an application table.

## Decision

Use `tokio-rusqlite` to serialize SQLite work on a dedicated connection thread. Keep transactions inside a single worker call so task transitions and their canonical events remain atomic.

Use `rusqlite_migration` for ordered, embedded SQL migrations. Migration state is recorded with SQLite's `user_version` pragma. The compatible `tokio-rusqlite` 0.7 and `rusqlite_migration` 2.3 releases share `rusqlite` 0.37 and preserve the workspace's Rust 1.91 minimum.

Existing unversioned databases are adopted safely because the baseline migrations use idempotent table creation before advancing `user_version`.

## Alternatives Considered

Wrapping every direct `rusqlite` call in `spawn_blocking` scatters scheduling policy and makes connection ownership harder to reason about. A full ORM adds a larger abstraction and compile-time surface without improving the current repository model.

## Consequences

Database work no longer blocks async executor threads, transaction boundaries remain explicit, and schema changes live as reviewable SQL files. The adapter remains responsible for mapping worker, SQLite, migration, and serialization errors into repository errors.

## Status

Accepted
