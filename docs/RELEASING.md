# Releasing voff

Use the auditable checklist in `docs/RELEASE_CHECKLIST.md` for every beta.

## Preconditions

- The working tree contains the intended release changes.
- `Cargo.lock` is committed and the version is updated consistently.
- Format, Clippy, tests, build, and dependency-policy checks pass.
- The release notes contain no credentials or private diagnostic data.

Run the local quality gate:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo bench -p voff-storage --bench large_history
cargo build --locked --release -p voff-cli
cargo deny check
```

## Creating a release

Create and push a signed or annotated semantic-version tag:

```bash
git tag -a v0.1.0-beta.1 -m "voff v0.1.0-beta.1"
git push origin v0.1.0-beta.1
```

The release workflow builds locked native binaries for Linux x86-64, macOS
x86-64, and macOS Apple Silicon. Each binary is packaged as a `.tar.gz` file
with a corresponding SHA-256 checksum. Tag builds publish the files to a GitHub
Release with generated notes. Manual workflow runs build artifacts without
publishing a release.

## Verifying an artifact

On Linux:

```bash
sha256sum -c voff-linux-x86_64.tar.gz.sha256
```

On macOS:

```bash
shasum -a 256 -c voff-macos-aarch64.tar.gz.sha256
```

After extracting, verify the binary before installation:

```bash
./voff --version
./voff init --help
```

Release archives include a project distributed under the user's choice of the
MIT License or Apache License 2.0. Verify both root license files and Cargo
metadata are present before publishing.
