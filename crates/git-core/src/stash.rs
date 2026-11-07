//! Stashes: work set aside to come back to. A stash is a commit (its parents are the
//! commit it was made on, the index, and, when untracked files were included, a third
//! commit holding them), so its files and diffs reuse the commit machinery.

use crate::{
    CommitFile, CommitInfo, DiffOptions, Error, FileDiff, Repo,
    history::{LOG_FORMAT, parse_log},
    runner::{run_git, run_git_hookless_env},
};

/// Runs a `git stash` subcommand. Stash builds its own pathspecs internally (to find and
/// clean untracked files), and the runner's usual `GIT_LITERAL_PATHSPECS=1` makes git read
/// them literally, so untracked files get left behind. No pathspec here comes from the
/// user, so literal mode is safe to turn off. Hooks are off too: stash writes a ref, which
/// would run the repository's `reference-transaction` hook even in an untrusted repo.
fn run_stash(repo: &Repo, args: &[&str]) -> Result<Vec<u8>, Error> {
    run_git_hookless_env(
        repo.root(),
        args,
        &[("GIT_LITERAL_PATHSPECS", "0".as_ref())],
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// Position in the list; 0 is the newest. Refers to `stash@{index}`.
    pub index: usize,
    /// The stash commit: tracked changes against the commit it was made on.
    pub commit: CommitInfo,
    /// The commit holding untracked files that were stashed too.
    pub untracked: Option<CommitInfo>,
    /// The branch it was made on.
    pub branch: String,
    /// What the user called it, or the commit it was made on if they didn't.
    pub message: String,
}

/// A file in a stash, and which of its two commits it lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashFile {
    pub file: CommitFile,
    pub untracked: bool,
}

/// Splits git's stash subject (`On main: my note`, or `WIP on main: 1a2b3c4 subject` when
/// no note was given) into the branch and a readable message.
pub fn parse_stash_subject(subject: &str) -> (String, String) {
    let (rest, auto) = match subject.strip_prefix("WIP on ") {
        Some(rest) => (rest, true),
        None => match subject.strip_prefix("On ") {
            Some(rest) => (rest, false),
            None => return (String::new(), subject.to_owned()),
        },
    };
    let Some((branch, message)) = rest.split_once(": ") else {
        return (String::new(), subject.to_owned());
    };
    // An automatic subject is "<short hash> <commit subject>"; the hash means nothing here.
    let message = if auto {
        message
            .split_once(' ')
            .map_or(message, |(_, subject)| subject)
    } else {
        message
    };
    (branch.to_owned(), message.to_owned())
}

impl Repo {
    /// All stashes, newest first.
    pub fn stashes(&self) -> Result<Vec<Stash>, Error> {
        let output = run_git(self.root(), &["stash", "list", LOG_FORMAT])?;
        parse_log(&output)?
            .into_iter()
            .enumerate()
            .map(|(index, commit)| {
                let untracked = match commit.parents.get(2) {
                    Some(id) => {
                        let output = run_git(self.root(), &["log", "-1", LOG_FORMAT, id])?;
                        parse_log(&output)?.into_iter().next()
                    }
                    None => None,
                };
                let (branch, message) = parse_stash_subject(&commit.subject);
                Ok(Stash {
                    index,
                    commit,
                    untracked,
                    branch,
                    message,
                })
            })
            .collect()
    }

    /// Sets aside every change, including untracked files, leaving the tree clean.
    /// An empty `message` lets git write its default.
    pub fn stash_push(&self, message: &str) -> Result<(), Error> {
        let message = message.trim();
        let mut args = vec!["stash", "push", "--include-untracked"];
        if !message.is_empty() {
            args.extend(["-m", message]);
        }
        let output = run_stash(self, &args)?;
        // Git exits successfully when there was nothing to save, and says so on stdout.
        if String::from_utf8_lossy(&output).contains("No local changes to save") {
            return Err(Error::Git {
                command: "stash".into(),
                stderr: "there are no changes to stash".into(),
            });
        }
        Ok(())
    }

    /// `stash@{n}` for `stash`, after checking it is still the stash the caller saw. The
    /// list can change underneath a screen (a terminal stashes too), and acting on
    /// "whatever is now number 2" would apply or destroy the wrong work.
    fn stash_ref(&self, stash: &Stash) -> Result<String, Error> {
        let name = format!("stash@{{{}}}", stash.index);
        let current = run_git(self.root(), &["rev-parse", "--verify", "--quiet", &name])
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned());
        match current {
            Ok(id) if id == stash.commit.id => Ok(name),
            _ => Err(Error::Git {
                command: "stash".into(),
                stderr: "the stash list changed since it was shown; reopen the list and try again"
                    .into(),
            }),
        }
    }

    /// Brings a stash's changes back, keeping the stash.
    pub fn stash_apply(&self, stash: &Stash) -> Result<(), Error> {
        let name = self.stash_ref(stash)?;
        run_stash(self, &["stash", "apply", "--index", &name]).map(|_| ())
    }

    /// Brings a stash's changes back and removes it. If applying fails the stash is kept.
    pub fn stash_pop(&self, stash: &Stash) -> Result<(), Error> {
        let name = self.stash_ref(stash)?;
        run_stash(self, &["stash", "pop", "--index", &name]).map(|_| ())
    }

    /// Throws a stash away.
    pub fn stash_drop(&self, stash: &Stash) -> Result<(), Error> {
        let name = self.stash_ref(stash)?;
        run_stash(self, &["stash", "drop", &name]).map(|_| ())
    }

    /// The files a stash holds: tracked changes, then untracked files.
    pub fn stash_files(&self, stash: &Stash) -> Result<Vec<StashFile>, Error> {
        let mut files: Vec<_> = self
            .commit_files(&stash.commit)?
            .into_iter()
            .map(|file| StashFile {
                file,
                untracked: false,
            })
            .collect();
        if let Some(untracked) = &stash.untracked {
            files.extend(
                self.commit_files(untracked)?
                    .into_iter()
                    .map(|file| StashFile {
                        file,
                        untracked: true,
                    }),
            );
        }
        Ok(files)
    }

    pub fn stash_diff(
        &self,
        stash: &Stash,
        file: &StashFile,
        options: DiffOptions,
    ) -> Result<FileDiff, Error> {
        let commit = match (&stash.untracked, file.untracked) {
            (Some(untracked), true) => untracked,
            _ => &stash.commit,
        };
        self.commit_diff(commit, &file.file, options)
    }
}
