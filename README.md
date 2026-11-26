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
- **History:** browse commits and their diffs with a commit graph, for the current branch or
  all branches at once, with branch and tag labels; revert, cherry-pick and merge. Undo the last
  commit while it is unpushed: its changes and message come back, ready to commit again.
- **Branches:** switch, create, merge, rename and delete from a searchable popover. Deleting a
  branch whose commits are on no other branch asks first.
- **Sync:** fetch, pull and push with progress you can cancel; failures show a clear message with
  Retry and, for a rejected push, "Pull, then push". The open repository is fetched in the
  background every five minutes.
- **Conflicts:** when a merge, cherry-pick or revert stops, resolve each conflicting region
  (ours, theirs or both) or edit the result, then continue or abort.
- **Stashes:** stash (including untracked files), apply, pop and drop.
- **Worktrees:** list the repository's other checkouts, open one, make a new one on a new
  branch, and remove one (it warns first about anything that would be lost). Switch between
  them from the worktree pill (⇧⌘B). Picking a branch another worktree has checked out opens
  that worktree, and the branch list marks those branches with the worktree's folder.
- **Repositories:** every repository you open or clone is remembered; switch between them from
  the repository pill (⌘T), and the last one reopens when Ramus starts.
- **Clone** any Git address, and a **command palette** (⌘K) for everything above.
- **GitHub sign-in** (OAuth device flow, token kept in the macOS keychain).
- **Light and dark:** Ramus follows macOS's appearance, switching when macOS does. To pick
  one regardless, choose "Appearance: Light", "Appearance: Dark" or "Appearance: Match macOS"
  in the command palette; the choice is remembered.
- **Update notice:** once a day Ramus asks GitHub for the latest release, and when it is newer
  an "Update · vX.Y.Z" pill appears in the title bar; clicking it opens the release page. Ramus
  never downloads or installs anything itself.

Not built yet: rebase, per-host repository lists for Clone.

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K | Command palette |
| ⌘O / ⇧⌘O | Open / clone a repository |
| ⌘T | Switch repository |
| ⌘B | Branch switcher |
| ⇧⌘B | Worktree switcher |
| ⌘1 / ⌘2 / ⌘3 / ⌘4 | Changes / History / Stashes / Worktrees |
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

### Contributing and releases

`main` is protected: every change arrives as a pull request (from a fork, for anyone who isn't
a maintainer), needs CI to pass and, for contributors, one approval. PRs are squash-merged, so
the **PR title** becomes the commit, and it must start with a type:

| Title starts with | Means | Release |
| --- | --- | --- |
| `feat:` | a new feature | next minor version |
| `fix:` | a bug fix | next patch version |
| `perf:` | faster, same behaviour | next patch version |
| `docs:` `ci:` `build:` `chore:` `refactor:` `style:` `test:` | everything else | none |

Releases are automatic. After each merge, [release-please](https://github.com/googleapis/release-please)
keeps one "chore(main): release x.y.z" PR open with the next version (in `Cargo.toml` and
`Cargo.lock`) and a `CHANGELOG.md` entry built from the merged PR titles. Changes that only touch
`.github/` or `docs/`, or whose titles don't release, never open or change it. **Merging that PR
ships the release**: it is tagged, the disk image is built on a macOS runner and attached, with a
checksum file, to a new GitHub release whose notes are the changelog.

The build job has read-only access; only a small separate job that runs no build code can write
the release. The finished files cross between the two through the Actions cache, not artifact
storage, so a full artifact quota cannot block a release. "Run workflow" on the Release workflow
builds the installer without publishing and keeps it for a day, so you can try a build first.

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
