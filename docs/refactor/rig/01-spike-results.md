# Rig refactor: Phase 1 spike results

Date: 2026-09-14

## Outcome

Rig 0.42.0 is selected and exactly pinned as a test-only `rig-core` dependency
with `default-features = false` and features `reqwest,rustls`. No production
provider route changed.

## Verified offline

- OpenAI Responses client and completion model construction
- Ollama client and completion model construction
- Unary and streaming completion future surfaces for both models
- Tool definition schema and local request validation
- Tool-call serialization and tool-result correlation
- Explicit stream cancellation within a 100 ms test bound
- Cancellation does not synthesize a successful terminal response
- Usage metadata is available, with Rig's documented zero sentinel before a
  terminal record
- OpenAI and Ollama model-listing surfaces compile
- Rig structured error/status inspection can feed one AgentMux mapping seam
- Stable domain/engine/storage/TUI manifests remain Rig-free

The tests make no network requests and contain no credentials. Live-provider
behavior, cassette parity and production event conversion are intentionally
Phase 2 work.

## Compatibility and cost

| Item | Result |
|---|---|
| AgentMux MSRV | Rust 1.91 |
| Tested compiler | rustc 1.91.1 |
| Rig manifest MSRV | not declared |
| Rig edition | 2024 |
| Selected crate/version | `rig-core = 0.42.0` |
| Enabled features | `reqwest`, `rustls` |
| Excluded | facade, `rig-agent`, derive, MCP, effect log, media/vector-store features |
| Normal/build provider tree | 145 unique lines |
| Tree including Rig dev dependency | 221 unique lines |
| New duplicate of concern | reqwest 0.12 legacy + reqwest 0.13 Rig |
| Release binary delta | 0 bytes while Rig is test-only |

## Quality gate

- `cargo fmt --all -- --check`: pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass
- `cargo test --workspace --all-features`: pass, 93 tests total, 0 failed
- `cargo deny check`: pass; duplicate-version warnings remain, including the
  expected temporary reqwest 0.12/0.13 split
- `git diff --check`: pass

## Exit-gate assessment

- Exact published dependency and lockfile: pass
- OpenAI and Ollama compile spike: pass
- Unary/stream/tool/cancel/usage/listing API evidence: pass
- Rig type-leakage guard: pass
- ADR with ownership, upgrade and rollback: pass
- Production parity: not claimed; belongs to Phase 2

## Phase 2 entry plan

1. Add provider characterization fixtures shared by legacy and Rig paths.
2. Introduce a private Rig adapter and a temporary internal selection switch.
3. Centralize expanded error mapping with table-driven tests.
4. Translate stream text/tool/usage/terminal items to existing AgentMux events.
5. Prove cancellation, malformed/truncated output and approval non-bypass.
6. Measure production dependency tree and release binary before enabling either
   provider route.
