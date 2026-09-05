# ADR 0007: Scoped read tools

## Context
Phase 1 permits read/search while writes and shell execution remain out of scope.
## Decision
Canonicalize the workspace root and every requested existing target, reject absolute/parent paths and sensitive components, skip symlinks, cap file size, file traversal, and result count, and implement literal text search. Phase 1 supplies the single agent with a bounded read-only snapshot (200 paths, 64 KiB) produced through this boundary; project content is explicitly labeled untrusted.
## Alternatives Considered
Passing arbitrary paths to shell utilities was rejected because it bypasses policy and adds command injection risk.
## Consequences
Tools are deterministic and read-only. Searches intentionally skip unreadable, binary, oversized, symlinked, `.git`, `.env`, and `target` content.
## Status
Accepted
