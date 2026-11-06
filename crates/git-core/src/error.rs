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
    #[error("unexpected git output: {0}")]
    Parse(String),
}
