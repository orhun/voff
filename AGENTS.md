# Repository instructions

Read `README.md` and the relevant code/tests before editing. Build voff as a safe, provider-neutral, terminal-first multi-agent system in stable Rust.

- Keep orchestration in `voff-engine`, domain types in `voff-core`, providers in `voff-provider`, SQLite in `voff-storage`, execution in `voff-tools`, and terminal UI in `crates/voff/src/tui`.
- Persist explicit tasks and delegation. Bound concurrency, depth, retries, and permissions.
- Keep approvals for destructive or external actions. Do not log secrets, prompts containing secrets, or authorization headers.
- Do not silently widen filesystem, network, shell, or delegation permissions.
- Keep the TUI responsive, keyboard-accessible, and legible on narrow terminals. Status must not rely on color alone.
- Preserve user changes. Add focused tests, including failure paths. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features` when possible.
