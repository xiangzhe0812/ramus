//! Worktrees: extra checkouts of the same repository, each in its own folder and
//! sharing one `.git`.

use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

use crate::{Error, Head, Repo, runner::run_git};

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
    })
}

fn unexpected(attribute: &[u8]) -> Error {
    Error::Parse(format!(
        "worktree list: {}",
        String::from_utf8_lossy(attribute)
    ))
}
