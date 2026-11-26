use std::collections::HashSet;

use crate::{
    DiffOptions, Error, FileDiff, OperationKind, Repo,
    diff::parse_diff,
    runner::{run_git, run_git_allowing, run_git_hookless},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub author: String,
    pub email: String,
    /// Commit time as Unix seconds.
    pub time: i64,
    pub parents: Vec<String>,
    pub subject: String,
    pub body: String,
}

impl CommitInfo {
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// One file changed by a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    /// The old path, for renames.
    pub original_path: Option<String>,
    /// Lines added and removed; `None` for a binary file.
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

const FIELD: char = '\0';

pub(crate) const LOG_FORMAT: &str = "--format=%H%x00%h%x00%an%x00%ae%x00%ct%x00%P%x00%s%x00%b%x00";

pub(crate) fn parse_log(output: &[u8]) -> Result<Vec<CommitInfo>, Error> {
    // Every field ends in NUL, the body included: NUL is the one byte a commit message
    // cannot contain, so a hostile message can't forge or split a record. (Record and unit
    // separators, by contrast, are legal in messages.) `git log` also puts a newline between
    // commits, which lands at the start of the next id.
    let text = String::from_utf8_lossy(output);
    let fields: Vec<&str> = text.split(FIELD).collect();
    let mut commits = Vec::new();
    for chunk in fields.chunks(8) {
        let [id, short_id, author, email, time, parents, subject, body] = chunk[..] else {
            // What follows the last NUL: nothing, or the log's trailing newline.
            if chunk.iter().all(|field| field.trim().is_empty()) {
                break;
            }
            return Err(Error::Parse(format!("bad log record {chunk:?}")));
        };
        let id = id.trim_start_matches('\n');
        // The id ends up as a command-line argument later, so it must be a plain hash.
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Parse(format!("bad commit id {id:?}")));
        }
        commits.push(CommitInfo {
            id: id.to_owned(),
            short_id: short_id.to_owned(),
            author: author.to_owned(),
            email: email.to_owned(),
            time: time
                .parse()
                .map_err(|_| Error::Parse(format!("bad commit time {time:?}")))?,
            parents: parents.split_whitespace().map(str::to_owned).collect(),
            subject: subject.to_owned(),
            body: body.trim().to_owned(),
        });
    }
    Ok(commits)
}

/// Parses `git diff --numstat -z`: `adds TAB dels TAB path NUL`, and for a rename
/// `adds TAB dels TAB NUL old NUL new NUL`.
pub(crate) fn parse_numstat(output: &[u8]) -> Result<Vec<CommitFile>, Error> {
    let text = String::from_utf8_lossy(output);
    let mut files = Vec::new();
    let mut raw = text.split('\0');
    while let Some(entry) = raw.next() {
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.splitn(3, '\t');
        let (Some(adds), Some(dels), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Parse(format!("bad numstat entry {entry:?}")));
        };
        let count = |value: &str| value.parse::<u32>().ok();
        let (path, original_path) = if path.is_empty() {
            // A rename: the old and new names follow as their own records.
            let old = raw.next().unwrap_or_default();
            let new = raw.next().unwrap_or_default();
            (new.to_owned(), Some(old.to_owned()))
        } else {
            (path.to_owned(), None)
        };
        files.push(CommitFile {
            path,
            original_path,
            additions: count(adds),
            deletions: count(dels),
        });
    }
    Ok(files)
}

impl Repo {
    /// Commits reachable from HEAD, newest first: `limit` of them after skipping `skip`.
    pub fn log(&self, skip: usize, limit: usize) -> Result<Vec<CommitInfo>, Error> {
        if run_git(self.root(), &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
            return Ok(Vec::new());
        }
        let skip = skip.to_string();
        let limit = limit.to_string();
        parse_log(&run_git(
            self.root(),
            &[
                "log",
                "--no-color",
                // Verifying signatures would run a program named by the repo's config.
                "--no-show-signature",
                LOG_FORMAT,
                "--skip",
                &skip,
                "--max-count",
                &limit,
            ],
        )?)
    }

    /// Ids of the commits on this branch that its upstream doesn't have yet. Empty when
    /// the branch has no upstream.
    pub fn unpushed_commits(&self) -> Result<HashSet<String>, Error> {
        let output = run_git_allowing(self.root(), &["rev-list", "@{upstream}..HEAD"], &[0, 128])?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// The files a commit changed. For a merge, what it changed relative to its first
    /// parent.
    pub fn commit_files(&self, commit: &CommitInfo) -> Result<Vec<CommitFile>, Error> {
        let output = if commit.is_merge() {
            let parent = format!("{}^1", commit.id);
            run_git(
                self.root(),
                &[
                    "diff",
                    "--numstat",
                    "-z",
                    "-M",
                    "--no-ext-diff",
                    "--no-textconv",
                    &parent,
                    &commit.id,
                ],
            )?
        } else {
            run_git(
                self.root(),
                &[
                    "diff-tree",
                    "--root",
                    "--no-commit-id",
                    "-r",
                    "-M",
                    "--numstat",
                    "-z",
                    "--no-ext-diff",
                    "--no-textconv",
                    &commit.id,
                ],
            )?
        };
        parse_numstat(&output)
    }

    /// One file's diff in a commit.
    pub fn commit_diff(
        &self,
        commit: &CommitInfo,
        file: &CommitFile,
        options: DiffOptions,
    ) -> Result<FileDiff, Error> {
        let parent = format!("{}^1", commit.id);
        let mut args = if commit.is_merge() {
            // Against the first parent, as `commit_files` does.
            vec!["diff", parent.as_str(), commit.id.as_str()]
        } else {
            // `--root` makes a first commit diff against the empty tree.
            vec![
                "diff-tree",
                "-p",
                "--root",
                "--no-commit-id",
                commit.id.as_str(),
            ]
        };
        args.extend([
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=dirty",
            "-U3",
            "-M",
        ]);
        if options.ignore_whitespace {
            args.push("-w");
        }
        args.push("--");
        args.extend(file.original_path.as_deref());
        args.push(&file.path);
        parse_diff(&run_git(self.root(), &args)?)
    }

    /// Whether the branch has an upstream to compare with: one is configured and its
    /// remote-tracking branch still exists (a branch deleted on the remote and pruned has
    /// none).
    pub fn has_upstream(&self) -> bool {
        run_git(
            self.root(),
            &["rev-parse", "--verify", "--quiet", "@{upstream}"],
        )
        .is_ok()
    }

    /// Takes back the last commit, `expected_head`: the branch moves to its parent, and
    /// the commit's changes stay staged. Returns the commit's summary and description, to
    /// commit again with. Refuses when HEAD is no longer `expected_head` (so the commit the
    /// user looked at is the one removed); while a merge, cherry-pick or revert is
    /// unfinished (a reset would silently end it); for the first commit, which has no
    /// parent; and for a commit the upstream already has, which only a revert can undo
    /// without rewriting shared history. Hooks are off, as for other ref writes allowed in
    /// an untrusted repository.
    pub fn undo_last_commit(&self, expected_head: &str) -> Result<(String, String), Error> {
        let head = run_git(self.root(), &["rev-parse", "--verify", "--quiet", "HEAD"])
            .map_err(|_| Error::NothingToUndo)?;
        let head = String::from_utf8_lossy(&head).trim().to_owned();
        if head != expected_head {
            return Err(Error::HeadMoved);
        }
        if self.in_progress()?.is_some() {
            return Err(Error::OperationInProgress);
        }
        if run_git(self.root(), &["rev-parse", "--verify", "--quiet", "HEAD^"]).is_err() {
            return Err(Error::NothingToUndo);
        }
        if self.has_upstream() && !self.unpushed_commits()?.contains(&head) {
            return Err(Error::AlreadyPushed);
        }
        let message = self.last_commit_message()?.ok_or(Error::NothingToUndo)?;
        run_git_hookless(self.root(), &["reset", "--soft", "HEAD~1"])?;
        Ok(message)
    }

    /// Adds a commit that undoes `commit`. If it conflicts, the revert stays in progress
    /// and `Error::Conflicts` names the files to resolve (or abort with
    /// [`Repo::abort_operation`]).
    pub fn revert(&self, commit: &CommitInfo) -> Result<(), Error> {
        self.replay(OperationKind::Revert, &["revert", "--no-edit"], commit)
    }

    /// Applies `commit`'s change on top of the current branch, with the same conflict
    /// behaviour as [`Repo::revert`].
    pub fn cherry_pick(&self, commit: &CommitInfo) -> Result<(), Error> {
        self.replay(OperationKind::CherryPick, &["cherry-pick"], commit)
    }

    fn replay(&self, kind: OperationKind, base: &[&str], commit: &CommitInfo) -> Result<(), Error> {
        let mut args: Vec<&str> = base.to_vec();
        if commit.is_merge() {
            args.extend(["-m", "1"]);
        }
        args.push(&commit.id);
        match run_git(self.root(), &args) {
            Ok(_) => Ok(()),
            // Conflicts stay for the user to resolve; any other failure is cleaned up.
            Err(error) => Err(self.stopped_or(kind, error)),
        }
    }

    /// Merges `branch` into the current branch. If it conflicts, the merge stays in
    /// progress for the user to resolve or abort.
    pub fn merge(&self, branch: &str) -> Result<(), Error> {
        if branch.is_empty() || branch.starts_with('-') {
            return Err(Error::InvalidBranchName(branch.to_owned()));
        }
        match run_git(self.root(), &["merge", "--no-edit", branch]) {
            Ok(_) => Ok(()),
            Err(error) => Err(self.stopped_or(OperationKind::Merge, error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, parents: &str, subject: &str, body: &str) -> String {
        format!(
            "{id}\0{}\0Alex\0a@x.io\0100\0{parents}\0{subject}\0{body}\0\n",
            id.get(..7).unwrap_or(id)
        )
    }

    #[test]
    fn an_id_that_is_not_a_plain_hash_is_rejected() {
        // It would be passed to git as an argument; `--output=…` must never get that far.
        for id in ["--output=/tmp/x", "-1", "HEAD", "zz", "abc def"] {
            let output = record(id, "", "s", "");
            assert!(parse_log(output.as_bytes()).is_err(), "{id}");
        }
    }

    #[test]
    fn parses_commits_with_multiline_bodies_and_parents() {
        let output = [
            record(
                "a".repeat(40).as_str(),
                "bbbb cccc",
                "Merge it",
                "line one\n\nline three\n",
            ),
            record("d".repeat(40).as_str(), "", "Root commit", ""),
        ]
        .concat();

        let commits = parse_log(output.as_bytes()).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "Merge it");
        assert_eq!(commits[0].body, "line one\n\nline three");
        assert!(commits[0].is_merge());
        assert_eq!(commits[0].parents, ["bbbb", "cccc"]);
        assert!(commits[1].parents.is_empty());
        assert!(!commits[1].is_merge());
        assert_eq!(commits[1].time, 100);
    }

    #[test]
    fn a_body_containing_odd_characters_does_not_break_parsing() {
        let output = record(
            &"a".repeat(40),
            "",
            "Subject",
            "tabs\tand \"quotes\" and unicode é",
        );
        let commits = parse_log(output.as_bytes()).unwrap();
        assert_eq!(commits[0].body, "tabs\tand \"quotes\" and unicode é");
    }

    #[test]
    fn empty_output_is_no_commits_and_garbage_is_an_error() {
        assert!(parse_log(b"").unwrap().is_empty());
        assert!(parse_log(b"not a record\x1e").is_err());
    }

    #[test]
    fn numstat_handles_plain_binary_and_renamed_files() {
        let output = b"3\t1\tsrc/a.rs\0-\t-\timg.png\x005\t0\t\0old name.txt\0new name.txt\0";
        let files = parse_numstat(output).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!((files[0].additions, files[0].deletions), (Some(3), Some(1)));
        assert_eq!(files[1].additions, None, "a binary file has no line counts");
        assert_eq!(files[2].path, "new name.txt");
        assert_eq!(files[2].original_path.as_deref(), Some("old name.txt"));
    }

    #[test]
    fn a_truncated_numstat_entry_is_an_error() {
        assert!(parse_numstat(b"3\0").is_err());
    }
}
