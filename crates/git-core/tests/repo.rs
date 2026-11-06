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

mod diff {
    use git_core::{DiffLineKind, DiffOptions, FileChange};

    use super::*;

    fn change(repo: &Repo, path: &str) -> FileChange {
        repo.status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} is not in the status"))
    }

    fn kinds(diff: &git_core::FileDiff) -> Vec<(DiffLineKind, &str)> {
        diff.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .map(|l| (l.kind, l.text.as_str()))
            .collect()
    }

    #[test]
    fn modified_file_shows_removed_and_added_lines() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\ntwo\nthree\n");
        fs::write(dir.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "a.txt"), DiffOptions::default())
            .unwrap();

        assert_eq!(
            kinds(&diff),
            [
                (DiffLineKind::Context, "one"),
                (DiffLineKind::Removed, "two"),
                (DiffLineKind::Added, "TWO"),
                (DiffLineKind::Context, "three"),
            ]
        );
        assert_eq!((diff.additions(), diff.deletions()), (1, 1));
    }

    #[test]
    fn untracked_file_is_all_additions() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("new.txt"), "x\ny\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "new.txt"), DiffOptions::default())
            .unwrap();

        assert_eq!(
            kinds(&diff),
            [(DiffLineKind::Added, "x"), (DiffLineKind::Added, "y")]
        );
        assert_eq!(diff.hunks[0].lines[1].new_number, Some(2));
    }

    #[test]
    fn deleted_file_is_all_removals() {
        let dir = new_repo();
        commit_file(dir.path(), "gone.txt", "bye\n");
        fs::remove_file(dir.path().join("gone.txt")).unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "gone.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!(kinds(&diff), [(DiffLineKind::Removed, "bye")]);
    }

    #[test]
    fn staged_and_unstaged_edits_show_together() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "1\n2\n3\n");
        fs::write(dir.path().join("a.txt"), "1\n2 staged\n3\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        fs::write(dir.path().join("a.txt"), "1\n2 staged\n3 unstaged\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "a.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!((diff.additions(), diff.deletions()), (2, 2));
    }

    #[test]
    fn staged_new_file_on_an_unborn_branch_can_be_diffed() {
        let dir = new_repo();
        fs::write(dir.path().join("first.txt"), "hello\n").unwrap();
        git(dir.path(), &["add", "first.txt"]);

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "first.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!(kinds(&diff), [(DiffLineKind::Added, "hello")]);
    }

    #[test]
    fn binary_files_are_flagged_not_dumped() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("img.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "img.bin"), DiffOptions::default())
            .unwrap();
        assert!(diff.binary);
        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn renamed_file_diffs_against_its_old_path() {
        let dir = new_repo();
        commit_file(dir.path(), "old.txt", "a\nb\nc\nd\ne\n");
        git(dir.path(), &["mv", "old.txt", "new.txt"]);
        fs::write(dir.path().join("new.txt"), "a\nb\nC\nd\ne\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "new.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!((diff.additions(), diff.deletions()), (1, 1));
    }

    #[test]
    fn ignore_whitespace_drops_whitespace_only_changes() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "keep\nspaced\n");
        fs::write(dir.path().join("a.txt"), "keep\n  spaced\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let file = change(&repo, "a.txt");
        assert_eq!(
            repo.diff(&file, DiffOptions::default())
                .unwrap()
                .additions(),
            1
        );
        let ignoring = DiffOptions {
            ignore_whitespace: true,
        };
        assert!(repo.diff(&file, ignoring).unwrap().hunks.is_empty());
    }

    #[test]
    fn paths_with_spaces_and_unicode_work() {
        let dir = new_repo();
        commit_file(dir.path(), "my notes é.txt", "a\n");
        fs::write(dir.path().join("my notes é.txt"), "b\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo
            .diff(&change(&repo, "my notes é.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!((diff.additions(), diff.deletions()), (1, 1));
    }
}

mod untrusted_repositories {
    use std::os::unix::fs::PermissionsExt;

    use git_core::{DiffOptions, FileChange};

    use super::*;

    /// A script that leaves a marker file behind when git runs it.
    fn marker_script(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let marker = dir.join("PWNED");
        let script = dir.join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (script, marker)
    }

    #[test]
    fn opening_and_reading_status_never_runs_a_repo_configured_fsmonitor() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let outside = TempDir::new().unwrap();
        let (script, marker) = marker_script(outside.path());
        git(
            dir.path(),
            &["config", "core.fsmonitor", script.to_str().unwrap()],
        );
        fs::write(dir.path().join("a.txt"), "changed\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        repo.status().unwrap();

        assert!(
            !marker.exists(),
            "git ran the repository's fsmonitor command"
        );
    }

    #[test]
    fn diffing_never_runs_a_repo_configured_textconv() {
        let dir = new_repo();
        let outside = TempDir::new().unwrap();
        let (script, marker) = marker_script(outside.path());
        fs::write(dir.path().join(".gitattributes"), "*.txt diff=evil\n").unwrap();
        git(dir.path(), &["add", ".gitattributes"]);
        commit_file(dir.path(), "a.txt", "a\n");
        git(
            dir.path(),
            &["config", "diff.evil.textconv", script.to_str().unwrap()],
        );
        fs::write(dir.path().join("a.txt"), "changed\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let change: FileChange = repo
            .status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == "a.txt")
            .unwrap();
        repo.diff(&change, DiffOptions::default()).unwrap();

        assert!(
            !marker.exists(),
            "git ran the repository's textconv command"
        );
    }
}
