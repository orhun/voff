# Contributing

Thanks for considering a contribution to Voff!

For a larger change or a change to the project's direction, open an issue
first so we can discuss the approach. For smaller changes, a focused pull
request is welcome.

## Issues

- Search existing issues before opening a new one.
- Use the issue templates for bug reports and feature requests.
- For bugs, include steps to reproduce, expected and actual behavior, your OS,
  and the Voff version or commit. Remove secrets from logs and configuration
  before sharing them.

## Pull requests

- Keep each pull request focused and explain the user-visible change.
- Add or update tests when behavior changes.
- Update documentation when needed.
- Run the checks below when possible and report any that you could not run.

## Development checks

Voff is a Rust workspace. Build and check it with stable Rust:

```bash
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## License

By contributing, you agree that your contributions may be distributed under
the terms of the project's [MIT](./LICENSE-MIT) or
[Apache 2.0](./LICENSE-APACHE) license.
