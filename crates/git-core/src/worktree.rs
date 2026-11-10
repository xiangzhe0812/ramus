//! Worktrees: extra checkouts of the same repository, each in its own folder and
//! sharing one `.git`.

use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

use crate::{
    Error, Head, Repo,
    branch::is_safe_branch_name,
    runner::{run_git, run_git_hookless},
};

/// Length of the abbreviated commit shown for a detached worktree.
const SHORT_SHA_LEN: usize = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The checked-out branch, or `Head::Detached` with the abbreviated commit.
    pub head: Head,
    /// The repository's main checkout, as opposed to a linked worktree.
    pub is_main: bool,
    pub locked: bool,
    /// git reports it prunable: its folder is gone.
    pub missing: bool,
    /// Full hash of the checked-out commit, kept for detached worktrees.
    oid: String,
}

/// What removing a worktree would throw away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemovalCheck {
    /// Changed, staged and untracked files. Ignored files are not counted.
    pub uncommitted_files: usize,
    /// Commits on its branch, or at its detached HEAD, that no other branch,
    /// remote-tracking branch or tag reaches.
    pub unique_commits: usize,
}

impl Repo {
    /// Every worktree of this repository, the main one first.
    pub fn worktrees(&self) -> Result<Vec<Worktree>, Error> {
        parse_worktrees(&run_git(
            self.root(),
            &["worktree", "list", "--porcelain", "-z"],
        )?)
    }

    /// Short name of the branch new worktrees start from, e.g. "origin/main" or "main":
    /// origin's default branch as of the last fetch, else a local main branch.
    pub fn default_base(&self) -> Result<String, Error> {
        if let Ok(output) = run_git(self.root(), &["rev-parse", "--abbrev-ref", "origin/HEAD"]) {
            let name = String::from_utf8_lossy(&output).trim().to_owned();
            if !name.is_empty() && name != "origin/HEAD" {
                return Ok(name);
            }
        }
        BASE_FALLBACKS
            .iter()
            .find(|full| {
                run_git(self.root(), &["rev-parse", "--verify", "--quiet", full]).is_ok()
            })
            .map(|full| short_ref_name(full))
            .ok_or(Error::NoDefaultBranch)
    }

    /// Creates `branch`, with no upstream, off [`Repo::default_base`] and checks it out in
    /// a new folder called `folder` next to the main worktree. Returns the new worktree's
    /// path. Runs the repository's hooks, so callers must check trust first.
    ///
    /// Everything is checked before git runs, so a refusal creates nothing.
    pub fn add_worktree(&self, branch: &str, folder: &str) -> Result<PathBuf, Error> {
        let branch = branch.trim();
        if !is_safe_branch_name(branch)
            || run_git(self.root(), &["check-ref-format", "--branch", branch]).is_err()
        {
            return Err(Error::InvalidBranchName(branch.to_owned()));
        }
        if !is_plain_folder_name(folder) {
            return Err(Error::InvalidFolderName(folder.to_owned()));
        }
        let main = self.main_worktree_path()?;
        let destination = main
            .parent()
            .ok_or_else(|| Error::InvalidFolderName(folder.to_owned()))?
            .join(folder);
        // `symlink_metadata`, so a dangling symbolic link counts as taken too.
        if destination.symlink_metadata().is_ok() {
            return Err(Error::FolderExists(destination));
        }
        let full_ref = format!("refs/heads/{branch}");
        if run_git(self.root(), &["rev-parse", "--verify", "--quiet", &full_ref]).is_ok() {
            return Err(Error::BranchExists(branch.to_owned()));
        }
        let base = self.default_base()?;
        // Relative to the main worktree, where git runs, so the path is always text.
        let relative = format!("../{folder}");
        run_git(
            &main,
            &["worktree", "add", "--no-track", "-b", branch, &relative, &base],
        )?;
        Ok(destination)
    }

    /// What removing `worktree` would lose. The same answer from any worktree.
    pub fn removal_check(&self, worktree: &Worktree) -> Result<RemovalCheck, Error> {
        let uncommitted_files = if worktree.missing {
            0
        } else {
            let output = run_git(
                &worktree.path,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )?;
            count_status_entries(&output)
        };
        Ok(RemovalCheck {
            uncommitted_files,
            unique_commits: self.unique_commits(worktree)?,
        })
    }

    /// Commits at the worktree's tip that no other branch, remote-tracking branch or tag
    /// reaches. `--all` is not used: it includes the current HEAD, which would hide the
    /// branch's own commits when asked from inside that worktree.
    fn unique_commits(&self, worktree: &Worktree) -> Result<usize, Error> {
        let (tip, exclude) = match &worktree.head {
            Head::Branch(name) => {
                // With `--branches`, `--exclude` takes the short name, not `refs/heads/…`.
                let exclude = format!("--exclude={name}");
                (format!("refs/heads/{name}"), Some(exclude))
            }
            Head::Detached(_) => (worktree.oid.clone(), None),
        };
        if run_git(self.root(), &["rev-parse", "--verify", "--quiet", &tip]).is_err() {
            // An unborn branch has no commits to lose.
            return Ok(0);
        }
        let mut args = vec!["rev-list", "--count", tip.as_str(), "--not"];
        args.extend(exclude.as_deref());
        args.extend(["--branches", "--remotes", "--tags"]);
        let output = run_git(self.root(), &args)?;
        String::from_utf8_lossy(&output)
            .trim()
            .parse()
            .map_err(|_| Error::Parse(format!("rev-list count: {}", String::from_utf8_lossy(&output))))
    }

    /// Removes a linked worktree's folder and git's record of it; `force` throws away
    /// uncommitted changes. A missing worktree only has its record cleared (with every
    /// other missing one). Runs from the main worktree, so the open worktree can be
    /// removed, and with hooks off, so it is safe in an untrusted repository. A locked
    /// worktree is refused by git.
    pub fn remove_worktree(&self, worktree: &Worktree, force: bool) -> Result<(), Error> {
        if worktree.is_main {
            return Err(Error::MainWorktree);
        }
        let main = self.main_worktree_path()?;
        if worktree.missing {
            return run_git_hookless(&main, &["worktree", "prune", "--expire=now"]).map(drop);
        }
        let path = worktree
            .path
            .to_str()
            .ok_or_else(|| Error::InvalidFolderName(worktree.path.display().to_string()))?;
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(path);
        run_git_hookless(&main, &args).map(drop)
    }

    /// Deletes a local branch even if it has unmerged commits. Hooks are off, as for
    /// other ref writes allowed in an untrusted repository.
    pub fn delete_branch(&self, name: &str) -> Result<(), Error> {
        if !is_safe_branch_name(name) {
            return Err(Error::InvalidBranchName(name.to_owned()));
        }
        run_git_hookless(self.root(), &["branch", "-D", name]).map(drop)
    }

    fn main_worktree_path(&self) -> Result<PathBuf, Error> {
        self.worktrees()?
            .into_iter()
            .find(|worktree| worktree.is_main)
            .map(|worktree| worktree.path)
            .ok_or_else(|| Error::Parse("worktree list has no main worktree".into()))
    }

    /// The git folder every worktree of this repository shares, absolute and canonical.
    pub fn common_dir(&self) -> Result<PathBuf, Error> {
        let output = run_git(
            self.root(),
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let dir = PathBuf::from(OsString::from_vec(trim_newline(output)));
        Ok(dir.canonicalize()?)
    }
}

/// Where new worktrees start when origin has no `HEAD`, in order of preference.
const BASE_FALLBACKS: [&str; 4] = [
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/heads/main",
    "refs/heads/master",
];

/// Counts entries in `git status --porcelain=v1 -z`. A rename or copy entry is followed
/// by an extra NUL-terminated source path, which is not an entry of its own.
fn count_status_entries(output: &[u8]) -> usize {
    let mut fields = output.split(|&byte| byte == 0).filter(|field| !field.is_empty());
    let mut count = 0;
    while let Some(field) = fields.next() {
        count += 1;
        if matches!(field.first(), Some(b'R' | b'C')) {
            fields.next();
        }
    }
    count
}

/// The default folder name for a new worktree's branch: "/" becomes "-".
pub fn worktree_folder_name(branch: &str) -> String {
    branch.replace('/', "-")
}

/// One path component that is neither special nor readable as a git option.
fn is_plain_folder_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('-')
        && !name.contains(['/', '\0'])
}

fn short_ref_name(full: &str) -> String {
    full.strip_prefix("refs/remotes/")
        .or_else(|| full.strip_prefix("refs/heads/"))
        .unwrap_or(full)
        .to_owned()
}

fn trim_newline(mut output: Vec<u8>) -> Vec<u8> {
    while output.last() == Some(&b'\n') {
        output.pop();
    }
    output
}

/// Parses `git worktree list --porcelain -z`: NUL-terminated attributes, with an empty
/// attribute ending each record. The first record is the main worktree.
fn parse_worktrees(output: &[u8]) -> Result<Vec<Worktree>, Error> {
    let mut worktrees = Vec::new();
    let mut record: Vec<&[u8]> = Vec::new();
    for attribute in output.split(|&byte| byte == 0) {
        if attribute.is_empty() {
            if !record.is_empty() {
                let is_main = worktrees.is_empty();
                worktrees.push(parse_record(&record, is_main)?);
                record.clear();
            }
        } else {
            record.push(attribute);
        }
    }
    if !record.is_empty() {
        let is_main = worktrees.is_empty();
        worktrees.push(parse_record(&record, is_main)?);
    }
    Ok(worktrees)
}

fn parse_record(attributes: &[&[u8]], is_main: bool) -> Result<Worktree, Error> {
    let mut path = None;
    let mut sha = None;
    let mut branch = None;
    let mut locked = false;
    let mut missing = false;
    for attribute in attributes {
        let (key, value) = match attribute.iter().position(|&byte| byte == b' ') {
            Some(space) => (&attribute[..space], &attribute[space + 1..]),
            None => (*attribute, &[][..]),
        };
        match key {
            b"worktree" => path = Some(PathBuf::from(OsString::from_vec(value.to_vec()))),
            b"HEAD" => sha = Some(String::from_utf8_lossy(value).into_owned()),
            b"branch" => {
                let name = value
                    .strip_prefix(b"refs/heads/")
                    .ok_or_else(|| unexpected(attribute))?;
                branch = Some(String::from_utf8_lossy(name).into_owned());
            }
            b"locked" => locked = true,
            b"prunable" => missing = true,
            _ => {}
        }
    }
    let (Some(path), Some(sha)) = (path, sha) else {
        return Err(unexpected(attributes.first().copied().unwrap_or_default()));
    };
    let head = match branch {
        Some(name) => Head::Branch(name),
        None => Head::Detached(sha.chars().take(SHORT_SHA_LEN).collect()),
    };
    Ok(Worktree {
        path,
        head,
        is_main,
        locked,
        missing,
        oid: sha,
    })
}

fn unexpected(attribute: &[u8]) -> Error {
    Error::Parse(format!(
        "worktree list: {}",
        String::from_utf8_lossy(attribute)
    ))
}
