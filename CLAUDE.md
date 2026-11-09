# Ramus

A Git desktop client in Rust and GPUI. Scope, stack and milestone order are settled in the maintainer's design notes (`docs/design-decisions.md`, kept out of the repository). If you don't have that file, ask before changing scope or the security model described in the README.

## Commands

- Build: `cargo build`
- Run the app: `cargo run -p ramus` (optionally `-- /path/to/repo` to open a repo directly)
- Test: `cargo test --workspace`
- Lint: `cargo clippy --workspace --all-targets` and `cargo fmt --all`

## Conventions

- `git-core` holds all Git logic and must not depend on `gpui`. The UI crate only renders state.
- Never hard-code colors in views; read them from the `Theme` global.
- GPUI is pinned to the `gpui-pre` snapshot that `gpui-component` requires (root `Cargo.toml`). Upgrade `gpui`, `gpui_platform` and `gpui-component` together, never separately.
- Text inputs come from `gpui-component`; `component_theme.rs` maps our `Theme` onto its theme. Don't write a text editor.
- Every git call goes through `git-core`'s runner, which neutralises repo-configured commands (fsmonitor, filters). Never spawn `git` directly from the UI crate.
- Build one milestone at a time and stop for review between milestones.
- The UI follows the maintainer's design canvas (private); match the existing views' layout and spacing.
