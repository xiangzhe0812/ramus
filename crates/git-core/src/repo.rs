use std::path::{Path, PathBuf};

use crate::{
    ChangeKind, Error, FileChange, FileDiff, RepoStatus,
    diff::parse_diff,
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
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
                "-z",
            ],
        )?;
        parse_status(&output)
    }

    /// The diff of one changed file against HEAD, so staged and unstaged edits show together.
    pub fn diff(&self, change: &FileChange, options: DiffOptions) -> Result<FileDiff, Error> {
        let mut args = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-U3",
        ];
        if options.ignore_whitespace {
            args.push("-w");
        }

        let base;
        let output = if change.worktree == Some(ChangeKind::Untracked) {
            // Git can't diff an untracked file against HEAD; diff it against nothing.
            args.extend(["--no-index", "--", "/dev/null", &change.path]);
            // `--no-index` exits 1 when the files differ, which is always the case here.
            run_git_allowing(&self.root, &args, &[0, 1])?
        } else {
            base = self.diff_base()?;
            args.extend(["-M", &base, "--"]);
            if let Some(original) = &change.original_path {
                args.push(original);
            }
            args.push(&change.path);
            run_git(&self.root, &args)?
        };

        if output.len() > MAX_DIFF_BYTES {
            return Err(Error::DiffTooLarge {
                limit_mib: MAX_DIFF_BYTES / (1024 * 1024),
            });
        }
        parse_diff(&output)
    }

    /// `HEAD`, or the empty tree on a branch with no commits yet.
    fn diff_base(&self) -> Result<String, Error> {
        if run_git(&self.root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok() {
            return Ok("HEAD".to_owned());
        }
        // stdin is empty, so this hashes the empty tree in whichever hash format the repo uses.
        let tree = run_git(&self.root, &["hash-object", "-t", "tree", "--stdin"])?;
        Ok(String::from_utf8_lossy(&tree).trim().to_owned())
    }

    /// Stages every change to `paths`, including deletions and untracked files.
    pub fn stage(&self, paths: &[&str]) -> Result<(), Error> {
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths);
        run_git(&self.root, &args).map(drop)
    }

    /// Stages every change in the working tree.
    pub fn stage_all(&self) -> Result<(), Error> {
        run_git(&self.root, &["add", "-A"]).map(drop)
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
}
