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
