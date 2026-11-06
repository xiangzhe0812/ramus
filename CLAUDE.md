# Ramus

A Git desktop client in Rust and GPUI. Read `docs/design-decisions.md` before changing anything: scope, stack and milestone order are settled there.

## Commands

- Build: `cargo build`
- Run the app: `cargo run -p ramus` (optionally `-- /path/to/repo` to open a repo directly)
- Test: `cargo test --workspace`
- Lint: `cargo clippy --workspace --all-targets` and `cargo fmt --all`

## Conventions

- `git-core` holds all Git logic and must not depend on `gpui`. The UI crate only renders state.
- Reads use `gix`; writes and network operations shell out to the `git` binary.
- Never hard-code colors in views; read them from the `Theme` global.
- GPUI is pinned to a Zed commit in the root `Cargo.toml`. Do not bump it casually.
- Build one milestone at a time and stop for review between milestones.
- UI design lives in the canvas linked from `docs/design-decisions.md`; match it.
