use std::path::{Path, PathBuf};

use crate::{Error, RepoStatus, runner::run_git, status::parse_status};

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
}
