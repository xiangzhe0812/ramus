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
    #[error("cancelled")]
    Cancelled,
    #[error("this repository has no remote to publish to")]
    NoRemote,
    #[error("this repository has several remotes and none is called origin ({}); set an upstream from a terminal", .0.join(", "))]
    AmbiguousRemote(Vec<String>),
    #[error("unexpected git output: {0}")]
    Parse(String),
}
