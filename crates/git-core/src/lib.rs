//! UI-independent Git logic: status, diff parsing, patch building, and the
//! `git` CLI runner. Nothing in this crate may depend on `gpui`.

mod diff;
mod error;
mod repo;
mod runner;
mod status;

pub use diff::{DiffHunk, DiffLine, DiffLineKind, FileDiff, FileDiffs};
pub use error::Error;
pub use repo::{DiffOptions, Repo};
pub use status::{BranchStatus, ChangeKind, FileChange, Head, RepoStatus};
