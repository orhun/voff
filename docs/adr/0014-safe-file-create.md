# ADR 0014: Approval-gated file creation

## Status

Accepted for Phase 5A.

## Problem

`file.patch` requires an existing canonical path, so an agent cannot safely
bootstrap an empty project. Treating a missing file as an empty patch target
would blur create and replace semantics and could overwrite a file created
while approval is pending.

## Decision

Add a distinct `file.create` capability. A proposal contains a relative path,
UTF-8 content, an explicit `expected_absent = true` precondition, a privacy
classification, the producing task, and a SHA-256 content digest. The tool
canonicalizes the existing parent directory, rejects traversal and sensitive
paths, enforces the workspace size limit, and publishes the complete addition
as an approval preview.

Approved execution writes a uniquely named sibling temporary file, syncs it,
and atomically links it into place without overwrite. If the destination was
created after proposal time, execution returns a create conflict. Approval and
artifact records remain in the existing SQLite audit model, so restart does
not bypass authorization.

## Consequences

- Empty projects can be bootstrapped without pre-creating placeholder files.
- Existing files can never be replaced through `file.create`.
- Parent directories must already exist; recursive directory creation remains
  a separate future capability and permission decision.
- The first version accepts UTF-8 text only and uses the existing one MiB
  workspace file limit.

## Alternatives rejected

- Extending `file.patch` with implicit create semantics: ambiguous review and
  unsafe races.
- General filesystem or shell access: unnecessarily broad authority.
- Creating missing parent directories automatically: expands the side effect
  beyond the single reviewed target.

## Test plan

Unit tests cover approval, traversal, size, destination races and atomic
creation. An engine integration test covers proposal, artifact preview,
persisted approval/audit and successful completion.
