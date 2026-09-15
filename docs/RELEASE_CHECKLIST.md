# Beta release checklist

## Prepare

- [ ] Confirm workspace version and tag are both `0.1.0-beta.1`.
- [ ] Move relevant Unreleased entries under the dated beta heading.
- [ ] Confirm `LICENSE-MIT`, `LICENSE-APACHE`, `SECURITY.md`, and
      `CODE_OF_CONDUCT.md` are included.
- [ ] Ensure no `.voff` state, diagnostics, credentials, or local artifacts
      are tracked.

## Quality gate

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-features`
- [ ] `cargo bench -p voff-storage --bench large_history`
- [ ] `cargo deny check`
- [ ] `cargo build --locked --release -p voff-cli`

## GitHub dry run

- [ ] Run the Release workflow with `workflow_dispatch` from the intended commit.
- [ ] Confirm all three build jobs pass.
- [ ] Download all workflow artifacts before tagging.
- [ ] Confirm a manual run did not publish a GitHub Release.

## Artifact verification

- [ ] Verify every `.sha256` file.
- [ ] Extract every archive and confirm it contains only the `voff` binary.
- [ ] Run `voff --version` and `voff init --help` on the matching OS and
      architecture.
- [ ] Confirm the binary reports `0.1.0-beta.1`.

## Publish

- [ ] Create annotated tag `v0.1.0-beta.1` from the verified commit.
- [ ] Push the tag and wait for the publish job.
- [ ] Confirm the GitHub Release contains three archives and three checksum files.
- [ ] Perform the documented solo and team tutorials from clean directories.
