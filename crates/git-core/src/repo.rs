use std::path::{Path, PathBuf};

use crate::{
    Branch, BranchKind, BranchList, ChangeKind, DiffHunk, Error, FileChange, FileDiff, FileDiffs,
    RepoStatus,
    branch::{LIST_ARGS, is_safe_branch_name, parse_branches},
    diff::{parse_diff, patch_for},
    runner::{run_git, run_git_allowing, run_git_with_input},
    status::parse_status,
};

/// Diffs bigger than this are refused rather than loaded into memory.
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffOptions {
    /// Ignore whitespace-only changes (`git diff -w`).
    pub ignore_whitespace: bool,
}

/// A handle to a Git working tree. Cheap to clone and holds no open state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Opens the repository containing `path`, which may be any directory inside it.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let output =
            run_git(path, &["rev-parse", "--show-toplevel"]).map_err(|error| match error {
                Error::Git { .. } => Error::NotARepository(path.to_owned()),
                other => other,
            })?;
        let root = String::from_utf8_lossy(&output).trim().to_owned();
        if root.is_empty() {
            return Err(Error::NotARepository(path.to_owned()));
        }
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder name, used as the repository's display name.
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    pub fn status(&self) -> Result<RepoStatus, Error> {
        let output = run_git(
            &self.root,
            &[
                "status",
                "--ignore-submodules=dirty",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
                "-z",
            ],
        )?;
        parse_status(&output)
    }

    /// A file's staged changes (HEAD → index) and unstaged changes (index → working tree).
    pub fn file_diffs(
        &self,
        change: &FileChange,
        options: DiffOptions,
    ) -> Result<FileDiffs, Error> {
        if change.worktree == Some(ChangeKind::Untracked) {
            // Git can't diff an untracked file against the index; diff it against nothing.
            // `--no-index` exits 1 when the files differ, which is always the case here.
            let unstaged = self.diff(
                options,
                &["--no-index", "--", "/dev/null", &change.path],
                &[0, 1],
            )?;
            return Ok(FileDiffs {
                staged: FileDiff::default(),
                unstaged,
            });
        }

        let unstaged = self.diff(options, &["--", &change.path], &[0])?;
        let mut staged_args = vec!["--cached", "-M", "--"];
        staged_args.extend(change.pathspecs());
        let staged = self.diff(options, &staged_args, &[0])?;
        Ok(FileDiffs { staged, unstaged })
    }

    fn diff(
        &self,
        options: DiffOptions,
        tail: &[&str],
        success_codes: &[i32],
    ) -> Result<FileDiff, Error> {
        let mut args = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=dirty",
            "-U3",
        ];
        if options.ignore_whitespace {
            args.push("-w");
        }
        args.extend(tail);

        let output = run_git_allowing(&self.root, &args, success_codes)?;
        if output.len() > MAX_DIFF_BYTES {
            return Err(Error::DiffTooLarge {
                limit_mib: MAX_DIFF_BYTES / (1024 * 1024),
            });
        }
        parse_diff(&output)
    }

    /// Stages just `hunks` of the unstaged changes to `path`. The hunks must come from
    /// a diff computed without `ignore_whitespace`, or their context won't match.
    pub fn stage_hunks(&self, path: &str, hunks: &[&DiffHunk]) -> Result<(), Error> {
        self.apply_hunks(path, hunks, false)
    }

    /// Removes just `hunks` of the staged changes to `path` from the index.
    pub fn unstage_hunks(&self, path: &str, hunks: &[&DiffHunk]) -> Result<(), Error> {
        self.apply_hunks(path, hunks, true)
    }

    fn apply_hunks(&self, path: &str, hunks: &[&DiffHunk], reverse: bool) -> Result<(), Error> {
        let patch = patch_for(path, hunks);
        let mut args = vec!["apply", "--cached", "--whitespace=nowarn"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        run_git_with_input(&self.root, &args, patch.as_bytes()).map(drop)
    }

    /// Stages every change to `paths`, including deletions and untracked files.
    pub fn stage(&self, paths: &[&str]) -> Result<(), Error> {
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths);
        run_git(&self.root, &args).map(drop)
    }

    /// Stages every change in the working tree.
    ///
    /// This adds the paths our own status lists rather than running `git add -A`, because
    /// `add -A` checks every submodule for uncommitted work by running `git status` inside
    /// it, under that submodule's own config, which is not ours to trust.
    pub fn stage_all(&self) -> Result<(), Error> {
        // Chunked so a very large change never exceeds the operating system's argument limit.
        const PATHS_PER_COMMAND: usize = 500;
        let status = self.status()?;
        for chunk in status.files.chunks(PATHS_PER_COMMAND) {
            let paths: Vec<&str> = chunk.iter().flat_map(|file| file.pathspecs()).collect();
            self.stage(&paths)?;
        }
        Ok(())
    }

    /// Removes `paths` from the index, leaving the working tree alone. Works on a
    /// branch with no commits, where `git restore --staged` does not.
    pub fn unstage(&self, paths: &[&str]) -> Result<(), Error> {
        let mut args = vec!["reset", "-q", "--"];
        args.extend(paths);
        run_git(&self.root, &args).map(drop)
    }

    pub fn unstage_all(&self) -> Result<(), Error> {
        run_git(&self.root, &["reset", "-q"]).map(drop)
    }

    /// Commits what is staged. `summary` is the first line; `description` follows
    /// after a blank line and may be empty. The repository's own hooks run, as they
    /// would for `git commit` in a terminal.
    pub fn commit(&self, summary: &str, description: &str) -> Result<(), Error> {
        let mut message = summary.trim().to_owned();
        if !description.trim().is_empty() {
            message.push_str("\n\n");
            message.push_str(description.trim());
        }
        message.push('\n');
        run_git_with_input(&self.root, &["commit", "-q", "-F", "-"], message.as_bytes()).map(drop)
    }

    /// Local branches, and remote branches that have no local counterpart.
    pub fn branches(&self) -> Result<BranchList, Error> {
        parse_branches(&run_git(&self.root, LIST_ARGS)?)
    }

    /// Checks out `branch`. A remote-only branch gets a local branch that tracks it.
    /// Fails, changing nothing, if local changes would be overwritten.
    pub fn switch_branch(&self, branch: &Branch) -> Result<(), Error> {
        if !is_safe_branch_name(&branch.name) {
            return Err(Error::InvalidBranchName(branch.name.clone()));
        }
        let mut args = vec!["switch"];
        if branch.kind == BranchKind::Remote {
            args.push("--track");
        }
        args.push(&branch.name);
        run_git(&self.root, &args).map(drop)
    }

    /// Creates a branch at the current commit and switches to it.
    pub fn create_branch(&self, name: &str) -> Result<(), Error> {
        let name = name.trim();
        if !is_safe_branch_name(name)
            || run_git(&self.root, &["check-ref-format", "--branch", name]).is_err()
        {
            return Err(Error::InvalidBranchName(name.to_owned()));
        }
        run_git(&self.root, &["switch", "-c", name]).map(drop)
    }

    /// The summary and description of the last commit, or `None` on a branch with no
    /// commits.
    pub fn last_commit_message(&self) -> Result<Option<(String, String)>, Error> {
        if run_git(&self.root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
            return Ok(None);
        }
        let output = run_git(&self.root, &["log", "-1", "--format=%B"])?;
        let message = String::from_utf8_lossy(&output).into_owned();
        let (summary, description) = message.split_once('\n').unwrap_or((&message, ""));
        Ok(Some((
            summary.trim().to_owned(),
            description.trim().to_owned(),
        )))
    }

    /// Replaces the last commit with one that also contains what is staged, using this
    /// message. The repository's hooks run, as for any commit.
    pub fn amend(&self, summary: &str, description: &str) -> Result<(), Error> {
        let mut message = summary.trim().to_owned();
        if !description.trim().is_empty() {
            message.push_str("\n\n");
            message.push_str(description.trim());
        }
        message.push('\n');
        run_git_with_input(
            &self.root,
            &["commit", "--amend", "-q", "-F", "-"],
            message.as_bytes(),
        )
        .map(drop)
    }
}
