//! Worktrees: extra checkouts of the same repository, each in its own folder and
//! sharing one `.git`.

use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

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
    /// Its folder is gone.
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
    /// Every worktree of this repository, the main one first. A bare repository has no
    /// main one: its checkouts are all linked.
    pub fn worktrees(&self) -> Result<Vec<Worktree>, Error> {
        let mut worktrees = parse_worktrees(&run_git(
            self.root(),
            &["worktree", "list", "--porcelain", "-z"],
        )?)?;
        // git only calls a worktree prunable when it isn't locked, but a locked one on an
        // unplugged drive is just as absent.
        for worktree in &mut worktrees {
            worktree.missing |= worktree.path.symlink_metadata().is_err();
        }
        Ok(worktrees)
    }

    /// Short name of the branch new worktrees start from, e.g. "origin/main" or "main":
    /// origin's default branch as of the last fetch, else a local main branch.
    pub fn default_base(&self) -> Result<String, Error> {
        Ok(short_ref_name(&self.default_base_ref()?))
    }

    /// The full name of [`Repo::default_base`]. git is given this, never the short name,
    /// which a tag or branch of the same name could shadow.
    fn default_base_ref(&self) -> Result<String, Error> {
        let exists =
            |full: &str| run_git(self.root(), &["rev-parse", "--verify", "--quiet", full]).is_ok();
        if let Ok(output) = run_git(self.root(), &["symbolic-ref", "-q", ORIGIN_HEAD]) {
            let target = String::from_utf8_lossy(&output).trim().to_owned();
            if target.starts_with("refs/") && exists(&target) {
                return Ok(target);
            }
        }
        BASE_FALLBACKS
            .iter()
            .find(|full| exists(full))
            .map(|full| (*full).to_owned())
            .ok_or(Error::NoDefaultBranch)
    }

    /// A name git takes literally as a branch: `check-ref-format --branch` accepts
    /// `@{-1}` and prints the branch it expands to, which is a different branch.
    fn is_plain_branch_name(&self, name: &str) -> bool {
        is_safe_branch_name(name)
            && run_git(self.root(), &["check-ref-format", "--branch", name])
                .is_ok_and(|output| String::from_utf8_lossy(&output).trim() == name)
    }

    /// Creates `branch`, with no upstream, off [`Repo::default_base`] and checks it out in
    /// a new folder called `folder` next to the main worktree. Returns the new worktree's
    /// path. Runs the repository's hooks, so callers must check trust first.
    ///
    /// Everything is checked before git runs, so a refusal creates nothing. If git itself
    /// fails after making the worktree (a failing `post-checkout` hook does this), the
    /// error is [`Error::WorktreeCreatedWithError`], which carries the new path.
    pub fn add_worktree(&self, branch: &str, folder: &str) -> Result<PathBuf, Error> {
        let branch = branch.trim();
        if !self.is_plain_branch_name(branch) {
            return Err(Error::InvalidBranchName(branch.to_owned()));
        }
        if !is_plain_folder_name(folder) {
            return Err(Error::InvalidFolderName(folder.to_owned()));
        }
        let worktrees = self.worktrees()?;
        let main = main_path(&worktrees)?;
        let destination = main
            .parent()
            .ok_or_else(|| Error::InvalidFolderName(folder.to_owned()))?
            .join(folder);
        // `symlink_metadata`, so a dangling symbolic link counts as taken too.
        if destination.symlink_metadata().is_ok() {
            return Err(Error::FolderExists(destination));
        }
        // A worktree whose folder was deleted by hand is still on record: git would make
        // the branch and only then refuse the folder.
        if worktrees
            .iter()
            .any(|worktree| same_location(&worktree.path, &destination))
        {
            return Err(Error::WorktreeOnRecord(destination));
        }
        let full_ref = format!("refs/heads/{branch}");
        if run_git(
            self.root(),
            &["rev-parse", "--verify", "--quiet", &full_ref],
        )
        .is_ok()
        {
            return Err(Error::BranchExists(branch.to_owned()));
        }
        let base = self.default_base_ref()?;
        // Relative to the main worktree, where git runs, so the path is always text.
        let relative = format!("../{folder}");
        let added = run_git(
            &main,
            &[
                "worktree",
                "add",
                "--no-track",
                "-b",
                branch,
                &relative,
                &base,
            ],
        );
        match added {
            Ok(_) => Ok(destination),
            // A failing post-checkout hook, for one, makes git exit non-zero after the
            // worktree is made. Say so, with its path, rather than claim nothing happened.
            Err(Error::Git { stderr, .. }) if self.is_registered(&destination) => {
                Err(Error::WorktreeCreatedWithError {
                    path: destination,
                    stderr,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Whether git lists an existing worktree at `path`. A listing that fails counts as
    /// no, so it never replaces the error being reported.
    fn is_registered(&self, path: &Path) -> bool {
        path.exists()
            && self.worktrees().is_ok_and(|worktrees| {
                worktrees
                    .iter()
                    .any(|worktree| same_location(&worktree.path, path))
            })
    }

    /// What removing `worktree` would lose. The same answer from any worktree.
    pub fn removal_check(&self, worktree: &Worktree) -> Result<RemovalCheck, Error> {
        let uncommitted_files = if worktree.missing {
            0
        } else {
            uncommitted_files(worktree)?
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
            .map_err(|_| {
                Error::Parse(format!(
                    "rev-list count: {}",
                    String::from_utf8_lossy(&output)
                ))
            })
    }

    /// Removes a linked worktree's folder and git's record of it; `force` throws away
    /// uncommitted changes. A missing worktree only has its record cleared. Runs from the
    /// main worktree, so the open worktree can be removed, and with hooks off, so it is
    /// safe in an untrusted repository. A locked worktree is refused by git, and one with
    /// submodules is refused here.
    ///
    /// Returns the main worktree. Keep working through that: if `self` was opened from
    /// the worktree just removed, its folder no longer exists.
    pub fn remove_worktree(&self, worktree: &Worktree, force: bool) -> Result<Repo, Error> {
        if worktree.is_main {
            return Err(Error::MainWorktree);
        }
        if !worktree.missing {
            // The status check below can't see inside submodules (the runner keeps git
            // from running in them, under their own config), so their uncommitted work
            // would go unnoticed. git refuses such a worktree too, unless forced.
            if has_initialised_submodules(worktree)? {
                return Err(Error::HasSubmodules(worktree.path.clone()));
            }
            // git's own cleanliness check runs `git status` inside the worktree, where
            // the runner can't blank filters set in that worktree's own config. So the
            // check is done here, from the worktree's folder through the runner, and git
            // is told to skip it with `--force`. That doesn't unlock a locked worktree.
            if !force && uncommitted_files(worktree)? > 0 {
                return Err(Error::UncommittedChanges(worktree.path.clone()));
            }
        }
        let main = main_path(&self.worktrees()?)?;
        let path = worktree
            .path
            .to_str()
            .ok_or_else(|| Error::InvalidFolderName(worktree.path.display().to_string()))?;
        run_git_hookless(&main, &["worktree", "remove", "--force", path])?;
        Repo::open(&main)
    }

    /// Deletes a local branch even if it has unmerged commits. Hooks are off, as for
    /// other ref writes allowed in an untrusted repository.
    pub fn delete_branch(&self, name: &str) -> Result<(), Error> {
        if !self.is_plain_branch_name(name) {
            return Err(Error::InvalidBranchName(name.to_owned()));
        }
        run_git_hookless(self.root(), &["branch", "-D", name]).map(drop)
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

const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";

/// File mode of a submodule entry in the index.
const SUBMODULE_MODE: &[u8] = b"160000 ";

/// Where new worktrees start when origin has no `HEAD`, in order of preference.
const BASE_FALLBACKS: [&str; 4] = [
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/heads/main",
    "refs/heads/master",
];

fn main_path(worktrees: &[Worktree]) -> Result<PathBuf, Error> {
    worktrees
        .iter()
        .find(|worktree| worktree.is_main)
        .map(|worktree| worktree.path.clone())
        .ok_or_else(|| Error::Parse("worktree list has no main worktree".into()))
}

/// Whether two paths name the same folder, even when the folder itself is gone: the
/// same name inside the same parent folder.
fn same_location(a: &Path, b: &Path) -> bool {
    let parent = |path: &Path| path.parent().and_then(|parent| parent.canonicalize().ok());
    a.file_name() == b.file_name() && parent(a).is_some() && parent(a) == parent(b)
}

/// Whether `worktree` has a submodule that is checked out. Reads the index and looks for
/// each submodule's `.git`; never runs git inside a submodule.
fn has_initialised_submodules(worktree: &Worktree) -> Result<bool, Error> {
    let output = run_git(&worktree.path, &["ls-files", "--stage", "-z"])?;
    Ok(output
        .split(|&byte| byte == 0)
        .filter(|entry| entry.starts_with(SUBMODULE_MODE))
        .filter_map(|entry| {
            entry
                .iter()
                .position(|&byte| byte == b'\t')
                .map(|tab| &entry[tab + 1..])
        })
        .any(|path| {
            worktree
                .path
                .join(PathBuf::from(OsString::from_vec(path.to_vec())))
                .join(".git")
                .symlink_metadata()
                .is_ok()
        }))
}

/// Changed, staged and untracked files in `worktree`. Run from its own folder, so the
/// runner sees, and blanks, filters set in that worktree's config.
fn uncommitted_files(worktree: &Worktree) -> Result<usize, Error> {
    let output = run_git(
        &worktree.path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Ok(count_status_entries(&output))
}

/// Counts entries in `git status --porcelain=v1 -z`. A rename or copy entry is followed
/// by an extra NUL-terminated source path, which is not an entry of its own.
fn count_status_entries(output: &[u8]) -> usize {
    let mut fields = output
        .split(|&byte| byte == 0)
        .filter(|field| !field.is_empty());
    let mut count = 0;
    while let Some(field) = fields.next() {
        count += 1;
        // Either column can say renamed or copied: the index side (`R `) or, for an
        // intent-to-add file, the worktree side (` R`).
        if field
            .iter()
            .take(2)
            .any(|status| matches!(status, b'R' | b'C'))
        {
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
    // git lists the repository itself first: its main checkout, or, for a bare
    // repository, the bare folder, which is no worktree and is left out.
    let mut is_first = true;
    // The output ends with an empty attribute, which closes the last record.
    for attribute in output.split(|&byte| byte == 0) {
        if !attribute.is_empty() {
            record.push(attribute);
            continue;
        }
        if record.is_empty() {
            continue;
        }
        if !(is_first && record.contains(&&b"bare"[..])) {
            worktrees.push(parse_record(&record, is_first)?);
        }
        is_first = false;
        record.clear();
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
