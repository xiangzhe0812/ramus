use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{} is not inside a Git repository", .0.display())]
    NotARepository(PathBuf),
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {command} failed: {stderr}")]
    Git { command: String, stderr: String },
    #[error("the diff is larger than {limit_mib} MiB and was not loaded")]
    DiffTooLarge { limit_mib: usize },
    #[error("\"{0}\" is not a valid branch name")]
    InvalidBranchName(String),
    #[error("refusing to touch {0}: a folder on the way to it is a symbolic link")]
    UnsafePath(String),
    #[error("cancelled")]
    Cancelled,
    #[error("this repository has no remote to publish to")]
    NoRemote,
    #[error("this repository has several remotes and none is called origin ({}); set an upstream from a terminal", .0.join(", "))]
    AmbiguousRemote(Vec<String>),
    #[error("{} already exists and is not an empty folder", .0.display())]
    DestinationNotEmpty(PathBuf),
    #[error("\"{0}\" is not a repository address")]
    InvalidUrl(String),
    #[error("stopped with conflicts in {}", .0.join(", "))]
    Conflicts(Vec<String>),
    #[error("unexpected git output: {0}")]
    Parse(String),
}
