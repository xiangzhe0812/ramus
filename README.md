# Ramus

<p align="center"><img src="assets/icon/ramus-1024.png" width="160" alt="Ramus icon: an R whose leg ends in a commit dot"></p>

A Git desktop client for macOS, written in Rust with [GPUI](https://www.gpui.rs). It aims to
replace GitHub Desktop for daily work: fast, keyboard-friendly, and careful about what a
repository is allowed to run on your machine.

> **Status: early.** The code is covered by automated tests, but the app has had very little
> hands-on use so far. Expect rough edges, and don't rely on it for anything you can't recover.
> macOS only, Apple Silicon tested.

## What it does

- **Changes:** stage and unstage per file, per hunk or per line, with syntax-coloured diffs; commit and amend; discard with undo.
- **History:** browse commits and their diffs; revert, cherry-pick and merge.
- **Branches:** switch, create and merge from a searchable popover.
- **Sync:** fetch, pull and push with progress you can cancel; failures show a clear message with
  Retry and, for a rejected push, "Pull, then push". The open repository is fetched in the
  background every five minutes.
- **Conflicts:** when a merge, cherry-pick or revert stops, resolve each conflicting region
  (ours, theirs or both) or edit the result, then continue or abort.
- **Stashes:** stash (including untracked files), apply, pop and drop.
- **Repositories:** every repository you open or clone is remembered; switch between them from
  the repository pill (⌘T), and the last one reopens when Ramus starts.
- **Clone** any Git address, and a **command palette** (⌘K) for everything above.
- **GitHub sign-in** (OAuth device flow, token kept in the macOS keychain).

Ramus deliberately has no commit-graph view. Not built yet: a light theme (dark only for now), rebase, per-host repository lists for Clone.

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K | Command palette |
| ⌘O / ⇧⌘O | Open / clone a repository |
| ⌘T | Switch repository |
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
  Ramus are trusted automatically. The choice is remembered per repository, and trusting any
  of its worktrees trusts them all.
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

## Install

Download the latest `Ramus-<version>-arm64.dmg` from the
[Releases](https://github.com/xiangzhe0812/ramus/releases) page (Apple Silicon, macOS 13 or
later), open it and drag **Ramus** onto **Applications**. Each release lists a SHA-256
checksum file next to the disk image.

The app is signed with an ad-hoc signature, not an Apple Developer ID, and is not notarized,
so macOS shows "Ramus can't be opened" the first time. Open it with **right-click → Open**
(or System Settings → Privacy & Security → **Open Anyway**). You only do this once.

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

### Building the installer

`packaging/macos/build.sh` builds a release binary and produces `dist/Ramus.app` and
`dist/Ramus-<version>-<arch>.dmg`. With a Developer ID certificate, sign it by setting
`CODESIGN_IDENTITY="Developer ID Application: …"`.

Releases are built by GitHub Actions (`.github/workflows/release.yml`). To publish one, set the
version in `Cargo.toml`, then tag and push it:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The workflow checks the tag matches the crate version, builds the disk image on a macOS runner and
attaches it, with a checksum file, to a new GitHub release, marked as the latest one. "Run workflow" on the Actions tab builds the installer without
publishing and keeps it for a day as a downloadable artifact, so you can try a build first. The
build job has read-only access; only a small separate job that runs no build code can write the
release. The finished files cross between the two through the Actions cache, not artifact storage,
so a full artifact quota cannot block a release.

### GitHub sign-in

Ramus ships with the client ID of its own GitHub OAuth app, so sign-in works out of the box
(⌘K → Accounts…). The client ID is public by design and no client secret is used. To use your
own app instead, for a fork or for development, create an OAuth app with **Device Flow**
enabled at `github.com/settings/developers` and start Ramus with its client ID:

```sh
RAMUS_GITHUB_CLIENT_ID=<your client id> cargo run -p ramus
```

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
