//! UI-independent Git logic: status, diff parsing, patch building, and the
//! `git` CLI runner. Nothing in this crate may depend on `gpui`.

mod branch;
mod diff;
mod discard;
mod error;
mod repo;
mod runner;
mod status;
mod sync;

pub use branch::{Branch, BranchKind, BranchList, relative_time};
pub use diff::{DiffHunk, DiffLine, DiffLineKind, FileDiff, FileDiffs};
pub use discard::DiscardBackup;
pub use error::Error;
pub use repo::{DiffOptions, Repo};
pub use status::{BranchStatus, ChangeKind, FileChange, Head, RepoStatus};
pub use sync::{CancelToken, Progress};
