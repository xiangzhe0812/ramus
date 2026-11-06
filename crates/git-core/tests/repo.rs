use std::{fs, path::Path, process::Command};

use git_core::{ChangeKind, Error, Head, Repo};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn new_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    dir
}

fn commit_file(dir: &Path, name: &str, contents: &str) {
    fs::write(dir.join(name), contents).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", &format!("add {name}")]);
}

#[test]
fn open_finds_the_root_from_a_subdirectory() {
    let dir = new_repo();
    fs::create_dir(dir.path().join("sub")).unwrap();

    let repo = Repo::open(&dir.path().join("sub")).unwrap();
    assert_eq!(
        repo.root().canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

#[test]
fn open_rejects_a_directory_that_is_not_a_repo() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        Repo::open(dir.path()),
        Err(Error::NotARepository(_))
    ));
}

#[test]
fn fresh_repo_is_an_unborn_branch_with_no_changes() {
    let dir = new_repo();
    let status = Repo::open(dir.path()).unwrap().status().unwrap();

    assert_eq!(status.branch.head, Head::Branch("main".into()));
    assert_eq!(status.branch.oid, None);
    assert!(status.files.is_empty());
}

#[test]
fn reports_modified_added_deleted_and_untracked_files() {
    let dir = new_repo();
    commit_file(dir.path(), "modified.txt", "one\n");
    commit_file(dir.path(), "deleted.txt", "bye\n");
    fs::write(dir.path().join("modified.txt"), "two\n").unwrap();
    fs::remove_file(dir.path().join("deleted.txt")).unwrap();
    fs::write(dir.path().join("staged.txt"), "new\n").unwrap();
    git(dir.path(), &["add", "staged.txt"]);
    fs::create_dir(dir.path().join("nested")).unwrap();
    fs::write(dir.path().join("nested/untracked.txt"), "x\n").unwrap();

    let status = Repo::open(dir.path()).unwrap().status().unwrap();
    let kinds: Vec<(&str, ChangeKind, bool)> = status
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind(), f.is_staged()))
        .collect();

    assert_eq!(
        kinds,
        [
            ("deleted.txt", ChangeKind::Deleted, false),
            ("modified.txt", ChangeKind::Modified, false),
            ("nested/untracked.txt", ChangeKind::Untracked, false),
            ("staged.txt", ChangeKind::Added, true),
        ]
    );
    assert!(status.branch.oid.is_some());
}

#[test]
fn detects_a_rename() {
    let dir = new_repo();
    commit_file(dir.path(), "old.txt", "some stable content\n");
    git(dir.path(), &["mv", "old.txt", "new.txt"]);

    let status = Repo::open(dir.path()).unwrap().status().unwrap();
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].path, "new.txt");
    assert_eq!(status.files[0].original_path.as_deref(), Some("old.txt"));
    assert_eq!(status.files[0].kind(), ChangeKind::Renamed);
}

#[test]
fn counts_commits_ahead_of_upstream() {
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);

    let dir = new_repo();
    commit_file(dir.path(), "a.txt", "a\n");
    git(
        dir.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git(dir.path(), &["push", "-q", "-u", "origin", "main"]);
    commit_file(dir.path(), "b.txt", "b\n");
    commit_file(dir.path(), "c.txt", "c\n");

    let status = Repo::open(dir.path()).unwrap().status().unwrap();
    assert_eq!(status.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!((status.branch.ahead, status.branch.behind), (2, 0));
}

#[test]
fn detached_head_reports_the_short_hash() {
    let dir = new_repo();
    commit_file(dir.path(), "a.txt", "a\n");
    git(dir.path(), &["checkout", "-q", "--detach"]);

    let status = Repo::open(dir.path()).unwrap().status().unwrap();
    assert!(matches!(status.branch.head, Head::Detached(ref sha) if sha.len() == 7));
}

#[test]
fn status_does_not_take_the_index_lock() {
    let dir = new_repo();
    commit_file(dir.path(), "a.txt", "a\n");
    fs::write(dir.path().join("a.txt"), "changed\n").unwrap();

    Repo::open(dir.path()).unwrap().status().unwrap();
    // A refresh must leave nothing behind that would break the user's own git commands.
    assert!(!dir.path().join(".git/index.lock").exists());
    git(dir.path(), &["add", "a.txt"]);
}
