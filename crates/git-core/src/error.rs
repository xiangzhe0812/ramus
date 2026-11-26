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
    #[error("\"{0}\" is not a valid folder name")]
    InvalidFolderName(String),
    #[error("{} already exists", .0.display())]
    FolderExists(PathBuf),
    #[error("a branch called \"{0}\" already exists")]
    BranchExists(String),
    #[error("{} has uncommitted changes", .0.display())]
    UncommittedChanges(PathBuf),
    #[error("created {}, but git reported a failure: {stderr}", .path.display())]
    WorktreeCreatedWithError { path: PathBuf, stderr: String },
    #[error("git still has a worktree on record at {}; remove that worktree first", .0.display())]
    WorktreeOnRecord(PathBuf),
    #[error("{} contains submodules, whose changes can't be checked here; remove it with git in a terminal", .0.display())]
    HasSubmodules(PathBuf),
    #[error("the main worktree can't be removed")]
    MainWorktree,
    #[error("found no main branch to start from (origin/HEAD, main or master)")]
    NoDefaultBranch,
    #[error("unexpected git output: {0}")]
    Parse(String),
    #[error("the first commit can't be undone")]
    NothingToUndo,
    #[error("the last commit is already pushed; revert it instead")]
    AlreadyPushed,
    #[error("finish or abort the merge, cherry-pick or revert first")]
    OperationInProgress,
    #[error("the last commit changed since it was shown; look again")]
    HeadMoved,
}
