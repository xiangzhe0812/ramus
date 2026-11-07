# Ramus

<p align="center"><img src="assets/icon/ramus-1024.png" width="160" alt="Ramus icon: a trunk branching into three"></p>

A Git desktop client for macOS, written in Rust with [GPUI](https://www.gpui.rs). It aims to
replace GitHub Desktop for daily work: fast, keyboard-friendly, and careful about what a
repository is allowed to run on your machine.

> **Status: early.** The code is covered by automated tests, but the app has had very little
> hands-on use so far. Expect rough edges, and don't rely on it for anything you can't recover.
> macOS only, Apple Silicon tested.

## What it does

- **Changes:** stage and unstage per file, per hunk or per line; commit and amend; discard with undo.
- **History:** browse commits and their diffs; revert, cherry-pick and merge.
- **Branches:** switch, create and merge from a searchable popover.
- **Sync:** fetch, pull and push with progress you can cancel; failures show a clear message with
  Retry and, for a rejected push, "Pull, then push". The open repository is fetched in the
  background every five minutes.
- **Conflicts:** when a merge, cherry-pick or revert stops, resolve each conflicting region
  (ours, theirs or both) or edit the result, then continue or abort.
- **Stashes:** stash (including untracked files), apply, pop and drop.
- **Clone** any Git address, and a **command palette** (⌘K) for everything above.
- **GitHub sign-in** (OAuth device flow, token kept in the macOS keychain).

Not built yet: the commit graph, a light theme, rebase, per-host repository lists for Clone.

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K | Command palette |
| ⌘O / ⇧⌘O | Open / clone a repository |
| ⌘B | Branch switcher |
| ⌘1 / ⌘2 / ⌘3 | Changes / History / Stashes |
| ⌘⌫ | Discard selected changes |
| ↑ ↓ Space | Move through files / include or exclude a file |

## Security model

A repository's own configuration is treated as untrusted, because opening a folder you
didn't write should not run its programs.

- A repository starts in **Restricted mode**. Viewing, diffs and staging work, but anything that
  would run the repository's hooks or helpers (commit, switching branches, fetch/push, merge,
  apply a stash) is refused until you click **Trust repository**. Repositories you clone through
  Ramus are trusted automatically; the choice is remembered per path.
- Every `git` call neutralises repo-configured `fsmonitor`, filter drivers, external diff and text
  conversion programs and signature verification, and never prompts for a password.
- Network commands only use the `file`, `git`, `http`, `https` and `ssh` protocols, so an `ext::`
  address cannot run a command.
- Actions that stay available in Restricted mode but write refs (stash, discard, abort) run with
  hooks disabled.
- Writes outside the usual git calls (discarding, resolving conflicts) refuse to follow symbolic links.

Don't paste addresses with a token or password in them (`https://user:token@host/…`): git would keep it in the repository's config. Use a credential helper or ssh keys.

Credentials are never stored by Ramus except the GitHub token, which lives in the keychain. Git
itself uses your normal credential helpers and `ssh-agent`.

## Building

You need macOS, a recent stable Rust toolchain (the native `aarch64-apple-darwin` one) and Xcode
with the Metal toolchain, which GPUI needs to compile its shaders:

```sh
xcodebuild -downloadComponent MetalToolchain
cargo run -p ramus                      # start the app
cargo run -p ramus -- /path/to/repo     # open a repository directly
cargo test --workspace
cargo clippy --workspace --all-targets
```

`git` must be on your `PATH`; Ramus shells out to it for everything that changes a repository. Use a current release: git itself ships fixes for hostile-repository bugs (symlink and `.git` tricks on clone) that Ramus relies on.

### GitHub sign-in

Sign-in needs a GitHub OAuth app with **Device Flow** enabled. Create one at
`github.com/settings/developers` and start Ramus with its client ID:

```sh
RAMUS_GITHUB_CLIENT_ID=<your client id> cargo run -p ramus
```

Until a client ID is set, the Accounts dialog explains this instead of contacting GitHub. The
client ID is not a secret; no client secret is used.

## Layout

- `crates/git-core`: all Git logic (status, diffs, staging, sync, stash, conflicts). It has no UI
  dependency and is tested against real temporary repositories.
- `crates/ramus`: the GPUI application. It only renders state and calls into `git-core`.

GPUI is pinned to the `gpui-pre` snapshot that `gpui-component` requires; upgrade `gpui`,
`gpui_platform` and `gpui-component` together.

## Icon

The artwork lives in `assets/icon/` as SVG; `assets/icon/build.sh` regenerates the PNGs, the macOS `Ramus.icns` and the GitHub social-preview card (needs `rsvg-convert` from `brew install librsvg`).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option.
