use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Once,
};

use git_core::{ChangeKind, Error, Head, RemovalCheck, Repo, Worktree, worktree_folder_name};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    isolate_from_host_git_config();
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Keeps the host's git config (aliases, filters, hooks paths) out of the tests.
/// Every test calls this before spawning git, so nothing reads the environment while
/// it is being changed.
fn isolate_from_host_git_config() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: runs once, before any test thread has spawned a process.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            // With no host config there is no identity, and commits need one.
            for role in ["AUTHOR", "COMMITTER"] {
                std::env::set_var(format!("GIT_{role}_NAME"), "Test");
                std::env::set_var(format!("GIT_{role}_EMAIL"), "test@example.com");
            }
            // An ssh that never answers, for the test that cancels a clone. Setting it here,
            // once, keeps the process environment untouched while tests run in parallel.
            let dir = TempDir::new().unwrap().keep();
            let slow_ssh = dir.join("slow-ssh");
            fs::write(&slow_ssh, "#!/bin/sh\nsleep 30\n").unwrap();
            fs::set_permissions(
                &slow_ssh,
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
            std::env::set_var("GIT_SSH_COMMAND", &slow_ssh);
        }
    });
}

fn new_repo() -> TempDir {
    isolate_from_host_git_config();
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
    isolate_from_host_git_config();
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
            .file_diffs(&change(&repo, "a.txt"), DiffOptions::default())
            .unwrap()
            .unstaged;

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
            .file_diffs(&change(&repo, "new.txt"), DiffOptions::default())
            .unwrap()
            .unstaged;

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
            .file_diffs(&change(&repo, "gone.txt"), DiffOptions::default())
            .unwrap()
            .unstaged;
        assert_eq!(kinds(&diff), [(DiffLineKind::Removed, "bye")]);
    }

    #[test]
    fn staged_and_unstaged_edits_are_reported_separately() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "1\n2\n3\n");
        fs::write(dir.path().join("a.txt"), "1\n2 staged\n3\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        fs::write(dir.path().join("a.txt"), "1\n2 staged\n3 unstaged\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diffs = repo
            .file_diffs(&change(&repo, "a.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!(
            kinds(&diffs.staged)[1..3],
            [
                (DiffLineKind::Removed, "2"),
                (DiffLineKind::Added, "2 staged"),
            ]
        );
        assert_eq!(
            kinds(&diffs.unstaged)[2..4],
            [
                (DiffLineKind::Removed, "3"),
                (DiffLineKind::Added, "3 unstaged"),
            ]
        );
        assert_eq!((diffs.additions(), diffs.deletions()), (2, 2));
    }

    #[test]
    fn staged_new_file_on_an_unborn_branch_can_be_diffed() {
        let dir = new_repo();
        fs::write(dir.path().join("first.txt"), "hello\n").unwrap();
        git(dir.path(), &["add", "first.txt"]);

        let repo = Repo::open(dir.path()).unwrap();
        let diffs = repo
            .file_diffs(&change(&repo, "first.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!(kinds(&diffs.staged), [(DiffLineKind::Added, "hello")]);
        assert!(diffs.unstaged.hunks.is_empty());
    }

    #[test]
    fn binary_files_are_flagged_not_dumped() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("img.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diffs = repo
            .file_diffs(&change(&repo, "img.bin"), DiffOptions::default())
            .unwrap();
        assert!(diffs.binary());
        assert!(diffs.is_empty());
    }

    #[test]
    fn renamed_file_diffs_against_its_old_path() {
        let dir = new_repo();
        commit_file(dir.path(), "old.txt", "a\nb\nc\nd\ne\n");
        git(dir.path(), &["mv", "old.txt", "new.txt"]);
        fs::write(dir.path().join("new.txt"), "a\nb\nC\nd\ne\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diffs = repo
            .file_diffs(&change(&repo, "new.txt"), DiffOptions::default())
            .unwrap();
        // The staged side is the pure rename; the later edit is unstaged.
        assert!(diffs.staged.hunks.is_empty());
        assert_eq!(
            (diffs.unstaged.additions(), diffs.unstaged.deletions()),
            (1, 1)
        );
    }

    #[test]
    fn ignore_whitespace_drops_whitespace_only_changes() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "keep\nspaced\n");
        fs::write(dir.path().join("a.txt"), "keep\n  spaced\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let file = change(&repo, "a.txt");
        assert_eq!(
            repo.file_diffs(&file, DiffOptions::default())
                .unwrap()
                .additions(),
            1
        );
        let ignoring = DiffOptions {
            ignore_whitespace: true,
        };
        assert!(repo.file_diffs(&file, ignoring).unwrap().is_empty());
    }

    #[test]
    fn paths_with_spaces_and_unicode_work() {
        let dir = new_repo();
        commit_file(dir.path(), "my notes é.txt", "a\n");
        fs::write(dir.path().join("my notes é.txt"), "b\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let diffs = repo
            .file_diffs(&change(&repo, "my notes é.txt"), DiffOptions::default())
            .unwrap();
        assert_eq!((diffs.additions(), diffs.deletions()), (1, 1));
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
        repo.file_diffs(&change, DiffOptions::default()).unwrap();

        assert!(
            !marker.exists(),
            "git ran the repository's textconv command"
        );
    }
}

mod staging {
    use super::*;

    fn status_of(repo: &Repo, path: &str) -> git_core::FileChange {
        repo.status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing from status"))
    }

    fn log_subjects(dir: &Path) -> Vec<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["log", "--format=%B%x00"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .split('\0')
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty())
            .collect()
    }

    #[test]
    fn stage_then_unstage_a_modified_file() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("a.txt"), "b\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.stage(&["a.txt"]).unwrap();
        assert!(status_of(&repo, "a.txt").is_fully_staged());

        repo.unstage(&["a.txt"]).unwrap();
        let after = status_of(&repo, "a.txt");
        assert!(!after.is_staged());
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "b\n");
    }

    #[test]
    fn staging_an_untracked_file_and_a_deletion() {
        let dir = new_repo();
        commit_file(dir.path(), "gone.txt", "x\n");
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        fs::write(dir.path().join("new.txt"), "n\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.stage(&["gone.txt", "new.txt"]).unwrap();
        assert!(status_of(&repo, "gone.txt").is_fully_staged());
        assert!(status_of(&repo, "new.txt").is_fully_staged());
    }

    #[test]
    fn a_partly_staged_file_is_staged_but_not_fully() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "1\n2\n");
        fs::write(dir.path().join("a.txt"), "1 staged\n2\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stage(&["a.txt"]).unwrap();
        fs::write(dir.path().join("a.txt"), "1 staged\n2 more\n").unwrap();

        let file = status_of(&repo, "a.txt");
        assert!(file.is_staged());
        assert!(!file.is_fully_staged());
    }

    #[test]
    fn unstaging_works_on_a_branch_with_no_commits() {
        let dir = new_repo();
        fs::write(dir.path().join("first.txt"), "hi\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stage(&["first.txt"]).unwrap();
        assert!(status_of(&repo, "first.txt").is_staged());

        repo.unstage(&["first.txt"]).unwrap();
        assert!(!status_of(&repo, "first.txt").is_staged());
    }

    #[test]
    fn unstaging_a_rename_restores_both_paths() {
        let dir = new_repo();
        commit_file(dir.path(), "old.txt", "stable content here\n");
        git(dir.path(), &["mv", "old.txt", "new.txt"]);
        let repo = Repo::open(dir.path()).unwrap();
        let renamed = status_of(&repo, "new.txt");
        assert!(renamed.is_staged());

        repo.unstage(&renamed.pathspecs()).unwrap();
        assert!(!status_of(&repo, "new.txt").is_staged());
        assert!(dir.path().join("new.txt").exists());
    }

    #[test]
    fn stage_all_handles_more_files_than_fit_in_one_command() {
        let dir = new_repo();
        commit_file(dir.path(), "base.txt", "b\n");
        for n in 0..1_234 {
            fs::write(dir.path().join(format!("file-{n}.txt")), "x\n").unwrap();
        }
        let repo = Repo::open(dir.path()).unwrap();

        repo.stage_all().unwrap();
        let files = repo.status().unwrap().files;
        assert_eq!(files.len(), 1_234);
        assert!(files.iter().all(|f| f.is_fully_staged()));
    }

    #[test]
    fn stage_all_and_unstage_all() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("a.txt"), "b\n").unwrap();
        fs::write(dir.path().join("new.txt"), "n\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.stage_all().unwrap();
        assert!(
            repo.status()
                .unwrap()
                .files
                .iter()
                .all(|f| f.is_fully_staged())
        );
        repo.unstage_all().unwrap();
        assert!(repo.status().unwrap().files.iter().all(|f| !f.is_staged()));
    }

    #[test]
    fn paths_with_glob_characters_mean_exactly_that_file() {
        let dir = new_repo();
        commit_file(dir.path(), "plain.txt", "p\n");
        fs::write(dir.path().join("a[1].txt"), "x\n").unwrap();
        fs::write(dir.path().join("a1.txt"), "y\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.stage(&["a[1].txt"]).unwrap();
        assert!(status_of(&repo, "a[1].txt").is_fully_staged());
        assert!(!status_of(&repo, "a1.txt").is_staged());

        let change = status_of(&repo, "a[1].txt");
        let diffs = repo
            .file_diffs(&change, git_core::DiffOptions::default())
            .unwrap();
        assert_eq!(diffs.additions(), 1);
    }

    #[test]
    fn commit_records_only_what_is_staged_with_summary_and_description() {
        let dir = new_repo();
        commit_file(dir.path(), "base.txt", "b\n");
        fs::write(dir.path().join("in.txt"), "in\n").unwrap();
        fs::write(dir.path().join("out.txt"), "out\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stage(&["in.txt"]).unwrap();

        repo.commit("  Add in.txt  ", "Because it matters.\n\nSecond paragraph.")
            .unwrap();

        assert_eq!(
            log_subjects(dir.path())[0],
            "Add in.txt\n\nBecause it matters.\n\nSecond paragraph."
        );
        let files = repo.status().unwrap().files;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "out.txt");
    }

    #[test]
    fn a_message_with_quotes_and_a_leading_dash_is_passed_through_intact() {
        let dir = new_repo();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stage_all().unwrap();

        repo.commit("-m \"quoted\" $(not run) `nor this`", "")
            .unwrap();
        assert_eq!(
            log_subjects(dir.path())[0],
            "-m \"quoted\" $(not run) `nor this`"
        );
    }

    #[test]
    fn commit_with_nothing_staged_is_an_error_with_gits_message() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();

        let error = repo.commit("empty", "").unwrap_err();
        assert!(matches!(error, Error::Git { .. }), "{error:?}");
    }

    #[test]
    fn a_failing_pre_commit_hook_blocks_the_commit_and_reports_its_output() {
        use std::os::unix::fs::PermissionsExt;

        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let hook = dir.path().join(".git/hooks/pre-commit");
        fs::write(
            &hook,
            "#!/bin/sh\necho 'lint failed: no trailing spaces' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stage(&["b.txt"]).unwrap();

        let error = repo.commit("blocked", "").unwrap_err().to_string();
        assert!(error.contains("lint failed"), "{error}");
        assert_eq!(log_subjects(dir.path()).len(), 1);
    }
}

mod untrusted_filters {
    use std::os::unix::fs::PermissionsExt;

    use git_core::DiffOptions;

    use super::*;

    /// A repo whose `.gitattributes` routes every `.txt` file through a clean filter
    /// that the repo's own config points at a script.
    fn repo_with_hostile_filter(outside: &Path, required: bool) -> (TempDir, std::path::PathBuf) {
        let marker = outside.join("PWNED");
        let script = outside.join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let dir = new_repo();
        fs::write(dir.path().join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        git(dir.path(), &["add", ".gitattributes"]);
        commit_file(dir.path(), "a.txt", "one\n");
        git(
            dir.path(),
            &["config", "filter.evil.clean", script.to_str().unwrap()],
        );
        if required {
            git(dir.path(), &["config", "filter.evil.required", "true"]);
        }
        // Same size but different content, so git can't tell from the stat alone and has
        // to run the file through the filter to compare it.
        fs::write(dir.path().join("a.txt"), "uno\n").unwrap();
        (dir, marker)
    }

    #[test]
    fn status_never_runs_a_repo_configured_clean_filter() {
        let outside = TempDir::new().unwrap();
        let (dir, marker) = repo_with_hostile_filter(outside.path(), false);

        Repo::open(dir.path()).unwrap().status().unwrap();
        assert!(!marker.exists(), "status ran the repository's clean filter");
    }

    #[test]
    fn the_harness_detects_a_filter_that_status_does_run_under_plain_git() {
        let outside = TempDir::new().unwrap();
        let (dir, marker) = repo_with_hostile_filter(outside.path(), false);

        git(dir.path(), &["status", "--porcelain"]);
        assert!(
            marker.exists(),
            "plain git status should run the repo's filter"
        );
    }

    #[test]
    fn diff_never_runs_a_repo_configured_clean_filter() {
        let outside = TempDir::new().unwrap();
        let (dir, marker) = repo_with_hostile_filter(outside.path(), false);
        let repo = Repo::open(dir.path()).unwrap();
        let change = repo.status().unwrap().files.into_iter().next().unwrap();

        repo.file_diffs(&change, DiffOptions::default()).unwrap();
        assert!(!marker.exists(), "diff ran the repository's clean filter");
    }

    #[test]
    fn staging_never_runs_a_repo_configured_clean_filter() {
        let outside = TempDir::new().unwrap();
        let (dir, marker) = repo_with_hostile_filter(outside.path(), false);

        Repo::open(dir.path()).unwrap().stage(&["a.txt"]).unwrap();
        assert!(
            !marker.exists(),
            "staging ran the repository's clean filter"
        );
    }

    #[test]
    fn a_required_filter_that_is_neutralised_does_not_make_git_fail() {
        let outside = TempDir::new().unwrap();
        let (dir, marker) = repo_with_hostile_filter(outside.path(), true);
        let repo = Repo::open(dir.path()).unwrap();

        let status = repo.status().unwrap();
        assert_eq!(status.files.len(), 1);
        assert!(!marker.exists());
    }
}

mod untrusted_filter_allowlist {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn marker_and_script(outside: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let marker = outside.join("PWNED");
        let script = outside.join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (script, marker)
    }

    #[test]
    fn a_filter_dressed_up_as_lfs_with_an_appended_command_does_not_run() {
        let outside = TempDir::new().unwrap();
        let (script, marker) = marker_and_script(outside.path());

        let dir = new_repo();
        fs::write(dir.path().join(".gitattributes"), "*.txt filter=lfs\n").unwrap();
        git(dir.path(), &["add", ".gitattributes"]);
        commit_file(dir.path(), "a.txt", "one\n");
        // Starts with the real LFS command, then chains the payload through the shell.
        git(
            dir.path(),
            &[
                "config",
                "filter.lfs.clean",
                &format!("git-lfs clean -- %f; {}", script.display()),
            ],
        );
        fs::write(dir.path().join("a.txt"), "one and a bit more\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let change = repo.status().unwrap().files.into_iter().next().unwrap();
        repo.file_diffs(&change, git_core::DiffOptions::default())
            .unwrap();
        repo.stage(&["a.txt"]).unwrap();

        assert!(!marker.exists(), "an LFS lookalike ran an appended command");
    }

    #[test]
    fn a_repo_whose_config_cannot_be_read_is_refused_rather_than_run_unprotected() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        // Not valid git config: every `git config` call now fails.
        let config = dir.path().join(".git/config");
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str("\n[filter \"broken\n");
        fs::write(&config, text).unwrap();

        assert!(Repo::open(dir.path()).is_err());
    }
}

mod unusual_filter_names {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, os::unix::fs::PermissionsExt};

    use super::*;

    /// Runs `status`, `diff` and `stage` on a repo whose hostile filter driver is called
    /// `driver` (raw bytes), and reports whether the payload ran.
    fn payload_ran(driver: &[u8]) -> bool {
        let outside = TempDir::new().unwrap();
        let marker = outside.path().join("PWNED");
        let script = outside.path().join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");

        let mut attributes = b"*.txt filter=".to_vec();
        attributes.extend_from_slice(driver);
        attributes.push(b'\n');
        fs::write(dir.path().join(".gitattributes"), attributes).unwrap();

        let mut key = b"filter.".to_vec();
        key.extend_from_slice(driver);
        key.extend_from_slice(b".clean");
        let configured = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .arg("config")
            .arg(OsString::from_vec(key))
            .arg(&script)
            .status()
            .unwrap();
        assert!(configured.success(), "could not set up the hostile filter");

        fs::write(dir.path().join("a.txt"), "one and a bit more\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let change = repo
            .status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == "a.txt")
            .unwrap();
        repo.file_diffs(&change, git_core::DiffOptions::default())
            .unwrap();
        repo.stage(&["a.txt"]).unwrap();
        marker.exists()
    }

    #[test]
    fn a_driver_name_containing_an_equals_sign_is_still_neutralised() {
        assert!(!payload_ran(b"evil=z"));
    }

    #[test]
    fn a_driver_name_that_is_not_valid_utf8_is_still_neutralised() {
        assert!(!payload_ran(b"bad\xffname"));
    }

    #[test]
    fn the_harness_does_detect_a_payload_that_does_run() {
        // Guards the two tests above against passing vacuously: run the same hostile
        // setup with git directly, bypassing the runner's defences.
        let outside = TempDir::new().unwrap();
        let marker = outside.path().join("PWNED");
        let script = outside.path().join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");
        fs::write(dir.path().join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        git(
            dir.path(),
            &["config", "filter.evil.clean", script.to_str().unwrap()],
        );
        fs::write(dir.path().join("a.txt"), "changed\n").unwrap();

        git(dir.path(), &["add", "a.txt"]);
        assert!(
            marker.exists(),
            "an unprotected `git add` should run the filter"
        );
    }
}

mod hunk_staging {
    use git_core::{DiffOptions, FileChange, FileDiffs};

    use super::*;

    fn twenty_lines() -> String {
        (1..=20).map(|n| format!("l{n}\n")).collect()
    }

    fn diffs(repo: &Repo, path: &str) -> (FileChange, FileDiffs) {
        let change = repo
            .status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing from status"));
        let diffs = repo.file_diffs(&change, DiffOptions::default()).unwrap();
        (change, diffs)
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8(out.stdout).unwrap()
    }

    /// A repo with `a.txt` committed, then changed near the top and near the bottom so
    /// the changes form two separate hunks.
    fn two_hunk_repo() -> (TempDir, Repo) {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", &twenty_lines());
        let edited = twenty_lines()
            .replace("l2\n", "l2 changed\n")
            .replace("l18\n", "l18 changed\n");
        fs::write(dir.path().join("a.txt"), edited).unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    fn hunk_text(hunk: &git_core::DiffHunk) -> String {
        hunk.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn staging_the_first_hunk_leaves_the_second_unstaged() {
        let (dir, repo) = two_hunk_repo();
        let (_, before) = diffs(&repo, "a.txt");
        assert_eq!(before.unstaged.hunks.len(), 2);

        repo.stage_hunks("a.txt", &[&before.unstaged.hunks[0]])
            .unwrap();

        let (change, after) = diffs(&repo, "a.txt");
        assert!(
            change.is_staged() && !change.is_fully_staged(),
            "partly staged"
        );
        assert_eq!(after.staged.hunks.len(), 1);
        assert!(hunk_text(&after.staged.hunks[0]).contains("l2 changed"));
        assert_eq!(after.unstaged.hunks.len(), 1);
        assert!(hunk_text(&after.unstaged.hunks[0]).contains("l18 changed"));
        // The working tree itself is untouched.
        assert!(
            fs::read_to_string(dir.path().join("a.txt"))
                .unwrap()
                .contains("l18 changed")
        );
    }

    #[test]
    fn staging_the_second_hunk_alone_works_even_when_the_first_added_lines() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", &twenty_lines());
        // Two inserted lines near the top shift every later line number in the working tree.
        let edited = twenty_lines()
            .replace("l2\n", "l2\nnew a\nnew b\n")
            .replace("l18\n", "l18 changed\n");
        fs::write(dir.path().join("a.txt"), edited).unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let (_, before) = diffs(&repo, "a.txt");

        repo.stage_hunks("a.txt", &[&before.unstaged.hunks[1]])
            .unwrap();

        let (_, after) = diffs(&repo, "a.txt");
        assert_eq!(after.staged.hunks.len(), 1);
        assert!(hunk_text(&after.staged.hunks[0]).contains("l18 changed"));
        assert!(!hunk_text(&after.staged.hunks[0]).contains("new a"));
        assert_eq!(after.unstaged.hunks.len(), 1);
    }

    #[test]
    fn unstaging_one_of_two_staged_hunks_keeps_the_other_staged() {
        let (_dir, repo) = two_hunk_repo();
        repo.stage(&["a.txt"]).unwrap();
        let (_, before) = diffs(&repo, "a.txt");
        assert_eq!(before.staged.hunks.len(), 2);

        repo.unstage_hunks("a.txt", &[&before.staged.hunks[0]])
            .unwrap();

        let (_, after) = diffs(&repo, "a.txt");
        assert_eq!(after.staged.hunks.len(), 1);
        assert!(hunk_text(&after.staged.hunks[0]).contains("l18 changed"));
        assert_eq!(after.unstaged.hunks.len(), 1);
        assert!(hunk_text(&after.unstaged.hunks[0]).contains("l2 changed"));
    }

    #[test]
    fn a_commit_after_staging_one_hunk_contains_only_that_hunk() {
        let (dir, repo) = two_hunk_repo();
        let (_, before) = diffs(&repo, "a.txt");
        repo.stage_hunks("a.txt", &[&before.unstaged.hunks[0]])
            .unwrap();

        repo.commit("Only the top change", "").unwrap();

        let shown = git_out(dir.path(), &["show", "HEAD", "--format=", "-U0"]);
        assert!(shown.contains("+l2 changed"), "{shown}");
        assert!(!shown.contains("l18 changed"), "{shown}");
        // The other hunk is still waiting in the working tree.
        let (_, after) = diffs(&repo, "a.txt");
        assert_eq!(after.unstaged.hunks.len(), 1);
        assert!(after.staged.hunks.is_empty());
    }

    #[test]
    fn staging_every_hunk_one_by_one_stages_the_whole_file() {
        let (_dir, repo) = two_hunk_repo();
        for _ in 0..2 {
            let (_, current) = diffs(&repo, "a.txt");
            repo.stage_hunks("a.txt", &[&current.unstaged.hunks[0]])
                .unwrap();
        }
        let (change, after) = diffs(&repo, "a.txt");
        assert!(change.is_fully_staged());
        assert!(after.unstaged.hunks.is_empty());
    }

    #[test]
    fn a_hunk_touching_a_file_without_a_trailing_newline_stages_exactly() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\ntwo");
        fs::write(dir.path().join("a.txt"), "one\ntwo changed").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let (_, before) = diffs(&repo, "a.txt");

        repo.stage_hunks("a.txt", &[&before.unstaged.hunks[0]])
            .unwrap();

        let (change, after) = diffs(&repo, "a.txt");
        assert!(
            change.is_fully_staged(),
            "index should now equal the working tree"
        );
        assert!(after.unstaged.hunks.is_empty());
        assert_eq!(git_out(dir.path(), &["show", ":a.txt"]), "one\ntwo changed");
    }

    #[test]
    fn file_names_with_spaces_quotes_and_backslashes_work() {
        for name in [
            "my notes é.txt",
            "q\"uote.txt",
            "back\\slash.txt",
            "tab\there.txt",
        ] {
            let dir = new_repo();
            commit_file(dir.path(), name, &twenty_lines());
            fs::write(
                dir.path().join(name),
                twenty_lines()
                    .replace("l2\n", "l2 changed\n")
                    .replace("l18\n", "l18 changed\n"),
            )
            .unwrap();
            let repo = Repo::open(dir.path()).unwrap();
            let (_, before) = diffs(&repo, name);

            repo.stage_hunks(name, &[&before.unstaged.hunks[0]])
                .unwrap_or_else(|e| panic!("{name:?}: {e}"));

            let (_, after) = diffs(&repo, name);
            assert_eq!(after.staged.hunks.len(), 1, "{name:?}");
            assert_eq!(after.unstaged.hunks.len(), 1, "{name:?}");
        }
    }

    #[test]
    fn a_stale_hunk_is_refused_and_the_index_is_left_alone() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", &twenty_lines());
        fs::write(
            dir.path().join("a.txt"),
            twenty_lines().replace("l2\n", "l2 changed\n"),
        )
        .unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let (_, before) = diffs(&repo, "a.txt");

        // The file changes under us after the diff was read: line 2 is staged differently.
        fs::write(
            dir.path().join("a.txt"),
            twenty_lines().replace("l2\n", "l2 DIFFERENT\n"),
        )
        .unwrap();
        git(dir.path(), &["add", "a.txt"]);
        let index_before = git_out(dir.path(), &["show", ":a.txt"]);

        let error = repo
            .stage_hunks("a.txt", &[&before.unstaged.hunks[0]])
            .unwrap_err();
        assert!(matches!(error, Error::Git { .. }), "{error:?}");
        assert_eq!(git_out(dir.path(), &["show", ":a.txt"]), index_before);
    }
}

mod branches {
    use git_core::BranchKind;

    use super::*;

    fn head_branch(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit_at(dir: &Path, message: &str, date: &str) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "--allow-empty", "-m", message])
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap();
        assert!(out.success());
    }

    #[test]
    fn lists_branches_newest_first_and_marks_the_current_one() {
        let dir = new_repo();
        commit_at(dir.path(), "old", "2024-01-01T00:00:00Z");
        git(dir.path(), &["branch", "older"]);
        git(dir.path(), &["switch", "-q", "-c", "newer"]);
        commit_at(dir.path(), "new", "2025-06-01T00:00:00Z");

        let list = Repo::open(dir.path()).unwrap().branches().unwrap();
        let names: Vec<_> = list.local.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["newer", "main", "older"]);
        let current: Vec<_> = list.local.iter().filter(|b| b.is_current).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].name, "newer");
        assert!(list.local[0].last_commit > list.local[2].last_commit);
    }

    #[test]
    fn a_fresh_repo_with_no_commits_lists_no_branches() {
        let dir = new_repo();
        let list = Repo::open(dir.path()).unwrap().branches().unwrap();
        assert!(list.local.is_empty() && list.remote_only.is_empty());
    }

    #[test]
    fn switching_checks_out_the_branch() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(dir.path(), &["branch", "other"]);
        let repo = Repo::open(dir.path()).unwrap();
        let other = repo
            .branches()
            .unwrap()
            .local
            .into_iter()
            .find(|b| b.name == "other")
            .unwrap();

        repo.switch_branch(&other).unwrap();
        assert_eq!(head_branch(dir.path()), "other");
    }

    #[test]
    fn a_remote_only_branch_is_listed_and_switching_creates_a_tracking_branch() {
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(
            dir.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(dir.path(), &["push", "-q", "origin", "main"]);
        git(dir.path(), &["branch", "teammate"]);
        git(dir.path(), &["push", "-q", "origin", "teammate"]);
        git(dir.path(), &["branch", "-D", "teammate"]);
        git(dir.path(), &["fetch", "-q", "origin"]);
        let repo = Repo::open(dir.path()).unwrap();

        let list = repo.branches().unwrap();
        let names: Vec<_> = list.remote_only.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["origin/teammate"], "origin/main has a local twin");
        assert_eq!(list.remote_only[0].kind, BranchKind::Remote);

        repo.switch_branch(&list.remote_only[0]).unwrap();
        assert_eq!(head_branch(dir.path()), "teammate");
        let after = repo.branches().unwrap();
        let tracking = after.local.iter().find(|b| b.name == "teammate").unwrap();
        assert_eq!(tracking.upstream.as_deref(), Some("origin/teammate"));
        assert!(after.remote_only.is_empty());
    }

    #[test]
    fn creating_a_branch_switches_to_it_at_the_current_commit() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();

        repo.create_branch("  feat/new-thing ").unwrap();
        assert_eq!(head_branch(dir.path()), "feat/new-thing");
    }

    #[test]
    fn creating_a_branch_keeps_uncommitted_work() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.create_branch("keep-going").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "edited\n"
        );
        assert_eq!(repo.status().unwrap().files.len(), 1);
    }

    #[test]
    fn invalid_branch_names_are_refused_before_git_sees_them() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();

        for name in [
            "",
            "  ",
            "has space",
            "a..b",
            "-force",
            "--detach",
            "x.lock",
            "a@{b",
            "end/",
            "~tilde",
        ] {
            let error = repo.create_branch(name).unwrap_err();
            assert!(
                matches!(error, Error::InvalidBranchName(_)),
                "{name:?}: {error:?}"
            );
        }
        assert_eq!(
            head_branch(dir.path()),
            "main",
            "nothing should have been created"
        );
    }

    #[test]
    fn creating_a_branch_that_exists_is_an_error() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(dir.path(), &["branch", "taken"]);
        let repo = Repo::open(dir.path()).unwrap();

        assert!(matches!(
            repo.create_branch("taken").unwrap_err(),
            Error::Git { .. }
        ));
        assert_eq!(head_branch(dir.path()), "main");
    }

    #[test]
    fn switching_that_would_overwrite_local_changes_fails_and_changes_nothing() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");
        git(dir.path(), &["switch", "-q", "-c", "other"]);
        commit_file(dir.path(), "a.txt", "two\n");
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "my unsaved edit\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let other = repo
            .branches()
            .unwrap()
            .local
            .into_iter()
            .find(|b| b.name == "other")
            .unwrap();

        let error = repo.switch_branch(&other).unwrap_err();
        assert!(error.to_string().contains("overwritten"), "{error}");
        assert_eq!(head_branch(dir.path()), "main");
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "my unsaved edit\n"
        );
    }

    #[test]
    fn a_branch_named_like_an_option_cannot_be_switched_to() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();
        let hostile = git_core::Branch {
            name: "--detach".into(),
            kind: BranchKind::Local,
            is_current: false,
            last_commit: None,
            upstream: None,
        };
        assert!(matches!(
            repo.switch_branch(&hostile),
            Err(Error::InvalidBranchName(_))
        ));
    }
}

mod untrusted_submodules {
    use std::os::unix::fs::PermissionsExt;

    use git_core::DiffOptions;

    use super::*;

    /// A superproject with one submodule whose own config names a hostile clean filter
    /// (under a driver name the superproject doesn't define) and whose working tree has a
    /// stat-dirty file, so git must run the filter to compare it.
    pub(super) fn superproject_with_hostile_submodule() -> (TempDir, TempDir, std::path::PathBuf) {
        let outside = TempDir::new().unwrap();
        let marker = outside.path().join("PWNED");
        let script = outside.path().join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let upstream = new_repo();
        commit_file(upstream.path(), "inner.txt", "one\n");

        let dir = new_repo();
        commit_file(dir.path(), "outer.txt", "outer\n");
        git(
            dir.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                upstream.path().to_str().unwrap(),
                "sub",
            ],
        );
        git(dir.path(), &["commit", "-q", "-m", "add submodule"]);

        let sub = dir.path().join("sub");
        fs::write(sub.join(".gitattributes"), "*.txt filter=subevil\n").unwrap();
        git(
            &sub,
            &["config", "filter.subevil.clean", script.to_str().unwrap()],
        );
        // Same size, different content: git has to run the file through the filter.
        fs::write(sub.join("inner.txt"), "uno\n").unwrap();
        (dir, outside, marker)
    }

    #[test]
    fn status_does_not_run_a_filter_configured_inside_a_submodule() {
        let (dir, _outside, marker) = superproject_with_hostile_submodule();
        Repo::open(dir.path()).unwrap().status().unwrap();
        assert!(
            !marker.exists(),
            "status ran a filter from the submodule's config"
        );
    }

    #[test]
    fn diff_does_not_run_a_filter_configured_inside_a_submodule() {
        let (dir, _outside, marker) = superproject_with_hostile_submodule();
        let repo = Repo::open(dir.path()).unwrap();
        // The submodule shows up as a changed entry in the superproject.
        let status = repo.status().unwrap();
        for change in &status.files {
            let _ = repo.file_diffs(change, DiffOptions::default());
        }
        assert!(
            !marker.exists(),
            "diff ran a filter from the submodule's config"
        );
    }

    #[test]
    fn staging_does_not_run_a_filter_configured_inside_a_submodule() {
        let (dir, _outside, marker) = superproject_with_hostile_submodule();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stage_all().unwrap();
        assert!(
            !marker.exists(),
            "staging ran a filter from the submodule's config"
        );
    }

    #[test]
    fn staging_a_submodule_by_path_does_not_run_its_filter_either() {
        let (dir, _outside, marker) = superproject_with_hostile_submodule();
        let sub = dir.path().join("sub");
        git(&sub, &["add", "-A"]);
        git(&sub, &["commit", "-q", "--no-verify", "-m", "advance"]);
        // The setup above ran the hostile filter itself (it is configured in `sub`), so
        // clear the marker, then dirty the submodule again with a same-size edit.
        fs::remove_file(&marker).unwrap();
        fs::write(sub.join("inner.txt"), "dos\n").unwrap();

        Repo::open(dir.path()).unwrap().stage(&["sub"]).unwrap();
        assert!(
            !marker.exists(),
            "staging the submodule path ran its filter"
        );
    }

    #[test]
    fn a_submodule_whose_commit_moved_still_shows_as_changed() {
        // Ignoring a submodule's working tree must not hide that it points somewhere new.
        let (dir, _outside, _marker) = superproject_with_hostile_submodule();
        let sub = dir.path().join("sub");
        git(&sub, &["add", "-A"]);
        git(
            &sub,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "advance",
            ],
        );

        let status = Repo::open(dir.path()).unwrap().status().unwrap();
        assert!(
            status.files.iter().any(|f| f.path == "sub"),
            "{:?}",
            status.files
        );
    }
}

mod submodule_control {
    use super::untrusted_submodules::superproject_with_hostile_submodule;
    use super::*;

    #[test]
    fn the_harness_detects_a_filter_that_does_run_under_plain_git() {
        let (dir, _outside, marker) = superproject_with_hostile_submodule();
        // No runner, no defences: this is what an unprotected `git status` would do.
        git(dir.path(), &["status", "--porcelain"]);
        assert!(
            marker.exists(),
            "plain git status should run the submodule's filter"
        );
    }
}

mod network {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use git_core::{CancelToken, Progress};

    use super::*;

    fn clone_of(remote: &Path) -> TempDir {
        let dir = TempDir::new().unwrap();
        let status = Command::new("git")
            .args(["clone", "-q"])
            .arg(remote)
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        git(dir.path(), &["checkout", "-q", "-B", "main", "origin/main"]);
        dir
    }

    /// A bare remote with one commit on `main`, plus two clones of it, `a` and `b`.
    fn remote_with_two_clones() -> (TempDir, TempDir, TempDir) {
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let seed = new_repo();
        commit_file(seed.path(), "seed.txt", "seed\n");
        git(
            seed.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(seed.path(), &["push", "-q", "origin", "main"]);
        let a = clone_of(remote.path());
        let b = clone_of(remote.path());
        (remote, a, b)
    }

    fn no_progress(_: Progress) {}

    fn behind_ahead(repo: &Repo) -> (u32, u32) {
        let branch = repo.status().unwrap().branch;
        (branch.behind, branch.ahead)
    }

    #[test]
    fn fetch_brings_in_new_commits_without_touching_the_working_tree() {
        let (_remote, a, b) = remote_with_two_clones();
        commit_file(b.path(), "from-b.txt", "b\n");
        git(b.path(), &["push", "-q", "origin", "main"]);
        let repo = Repo::open(a.path()).unwrap();
        assert_eq!(behind_ahead(&repo), (0, 0));

        repo.fetch(&CancelToken::new(), &mut no_progress).unwrap();

        assert_eq!(behind_ahead(&repo), (1, 0));
        assert!(
            !a.path().join("from-b.txt").exists(),
            "fetch must not change files"
        );
    }

    #[test]
    fn pull_fast_forwards_to_the_upstream() {
        let (_remote, a, b) = remote_with_two_clones();
        commit_file(b.path(), "from-b.txt", "b\n");
        git(b.path(), &["push", "-q", "origin", "main"]);
        let repo = Repo::open(a.path()).unwrap();

        repo.pull(&CancelToken::new(), &mut no_progress).unwrap();

        assert!(a.path().join("from-b.txt").exists());
        assert_eq!(behind_ahead(&repo), (0, 0));
    }

    #[test]
    fn pull_refuses_diverged_branches_and_changes_nothing() {
        let (_remote, a, b) = remote_with_two_clones();
        commit_file(b.path(), "from-b.txt", "b\n");
        git(b.path(), &["push", "-q", "origin", "main"]);
        commit_file(a.path(), "from-a.txt", "a\n");
        let repo = Repo::open(a.path()).unwrap();
        let head_before = git_head(a.path());

        let error = repo
            .pull(&CancelToken::new(), &mut no_progress)
            .unwrap_err();

        assert!(
            error.to_string().to_lowercase().contains("fast-forward"),
            "{error}"
        );
        assert_eq!(git_head(a.path()), head_before);
        assert!(!a.path().join("from-b.txt").exists());
    }

    fn git_head(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn push_sends_local_commits() {
        let (remote, a, _b) = remote_with_two_clones();
        commit_file(a.path(), "new.txt", "n\n");
        let repo = Repo::open(a.path()).unwrap();
        assert_eq!(behind_ahead(&repo), (0, 1));

        repo.push(&CancelToken::new(), &mut no_progress).unwrap();

        assert_eq!(behind_ahead(&repo), (0, 0));
        assert_eq!(git_head(remote.path()), git_head(a.path()));
    }

    #[test]
    fn a_rejected_push_reports_gits_explanation() {
        let (_remote, a, b) = remote_with_two_clones();
        commit_file(b.path(), "from-b.txt", "b\n");
        git(b.path(), &["push", "-q", "origin", "main"]);
        commit_file(a.path(), "from-a.txt", "a\n");
        let repo = Repo::open(a.path()).unwrap();

        let error = repo
            .push(&CancelToken::new(), &mut no_progress)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("rejected") || error.contains("failed to push"),
            "{error}"
        );
    }

    #[test]
    fn publishing_a_new_branch_sets_its_upstream() {
        let (remote, a, _b) = remote_with_two_clones();
        git(a.path(), &["switch", "-q", "-c", "feat/new"]);
        commit_file(a.path(), "feat.txt", "f\n");
        let repo = Repo::open(a.path()).unwrap();
        assert_eq!(repo.status().unwrap().branch.upstream, None);

        repo.publish(&CancelToken::new(), &mut no_progress).unwrap();

        let branch = repo.status().unwrap().branch;
        assert_eq!(branch.upstream.as_deref(), Some("origin/feat/new"));
        assert_eq!((branch.ahead, branch.behind), (0, 0));
        let on_remote = Command::new("git")
            .arg("-C")
            .arg(remote.path())
            .args(["branch", "--list", "feat/new"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&on_remote.stdout).contains("feat/new"));
    }

    #[test]
    fn publishing_without_any_remote_says_so() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();
        assert!(matches!(
            repo.publish(&CancelToken::new(), &mut no_progress),
            Err(Error::NoRemote)
        ));
    }

    #[test]
    fn publishing_with_several_remotes_and_no_origin_asks_the_user_to_choose() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(dir.path(), &["remote", "add", "one", "/nowhere/one"]);
        git(dir.path(), &["remote", "add", "two", "/nowhere/two"]);
        let repo = Repo::open(dir.path()).unwrap();
        assert!(matches!(
            repo.publish(&CancelToken::new(), &mut no_progress),
            Err(Error::AmbiguousRemote(_))
        ));
    }

    #[test]
    fn publishing_uses_the_only_remote_whatever_it_is_called() {
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(
            dir.path(),
            &["remote", "add", "upstream", remote.path().to_str().unwrap()],
        );
        let repo = Repo::open(dir.path()).unwrap();

        repo.publish(&CancelToken::new(), &mut no_progress).unwrap();
        assert_eq!(
            repo.status().unwrap().branch.upstream.as_deref(),
            Some("upstream/main")
        );
    }

    #[test]
    fn pushing_with_nothing_configured_fails_with_gits_message_instead_of_hanging() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();
        let started = Instant::now();

        let error = repo
            .push(&CancelToken::new(), &mut no_progress)
            .unwrap_err();

        assert!(matches!(error, Error::Git { .. }), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A pre-push hook that records its pid, then sleeps: a push that is stuck.
    fn stuck_push() -> (TempDir, TempDir, std::path::PathBuf) {
        let (_remote, a, _b) = remote_with_two_clones();
        let pid_file = a.path().join("hook.pid");
        let hook = a.path().join(".git/hooks/pre-push");
        fs::write(
            &hook,
            format!("#!/bin/sh\necho $$ > '{}'\nsleep 60\n", pid_file.display()),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        commit_file(a.path(), "new.txt", "n\n");
        (_remote, a, pid_file)
    }

    fn process_is_alive(pid: i32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn cancelling_stops_a_stuck_push_and_kills_the_processes_it_started() {
        let (_remote, a, pid_file) = stuck_push();
        let repo = Repo::open(a.path()).unwrap();
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            canceller.cancel();
        });

        let result = repo.push(&cancel, &mut no_progress);

        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "cancel must not wait for the hook"
        );
        let pid: i32 = fs::read_to_string(&pid_file)
            .expect("the hook should have started")
            .trim()
            .parse()
            .unwrap();
        // Give the signal a moment to land, then the hook's shell must be gone.
        let deadline = Instant::now() + Duration::from_secs(3);
        while process_is_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !process_is_alive(pid),
            "the hook outlived the cancelled push"
        );
    }

    #[test]
    fn a_token_that_is_already_cancelled_stops_the_operation_at_once() {
        let (_remote, a, _pid_file) = stuck_push();
        let repo = Repo::open(a.path()).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();

        assert!(matches!(
            repo.push(&cancel, &mut no_progress),
            Err(Error::Cancelled)
        ));
    }
}

mod discarding {
    use git_core::FileChange;

    use super::*;

    fn change(repo: &Repo, path: &str) -> FileChange {
        repo.status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} not in status"))
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap()
    }

    #[test]
    fn discarding_a_modified_file_restores_it_and_clears_its_staged_state() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "original\n");
        fs::write(dir.path().join("a.txt"), "staged edit\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        fs::write(dir.path().join("a.txt"), "staged edit\nplus unstaged\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.discard(&[&change(&repo, "a.txt")]).unwrap();

        assert_eq!(read(dir.path(), "a.txt"), "original\n");
        assert!(repo.status().unwrap().files.is_empty());
    }

    #[test]
    fn discarding_a_deleted_file_brings_it_back() {
        let dir = new_repo();
        commit_file(dir.path(), "gone.txt", "keep me\n");
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.discard(&[&change(&repo, "gone.txt")]).unwrap();
        assert_eq!(read(dir.path(), "gone.txt"), "keep me\n");
    }

    #[test]
    fn discarding_an_untracked_file_moves_it_aside_instead_of_deleting_it() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::create_dir_all(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested/new.txt"), "precious\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        let backup = repo.discard(&[&change(&repo, "nested/new.txt")]).unwrap();

        assert!(!dir.path().join("nested/new.txt").exists());
        let saved = dir
            .path()
            .join(".git/ramus/discarded")
            .join(&backup.id)
            .join("nested/new.txt");
        assert_eq!(fs::read_to_string(saved).unwrap(), "precious\n");
    }

    #[test]
    fn discarding_a_file_added_to_the_index_removes_it() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("added.txt"), "new\n").unwrap();
        git(dir.path(), &["add", "added.txt"]);
        let repo = Repo::open(dir.path()).unwrap();

        repo.discard(&[&change(&repo, "added.txt")]).unwrap();

        assert!(!dir.path().join("added.txt").exists());
        assert!(repo.status().unwrap().files.is_empty());
    }

    #[test]
    fn discarding_a_rename_puts_the_old_name_back() {
        let dir = new_repo();
        commit_file(dir.path(), "old.txt", "stable content here\n");
        git(dir.path(), &["mv", "old.txt", "new.txt"]);
        let repo = Repo::open(dir.path()).unwrap();

        repo.discard(&[&change(&repo, "new.txt")]).unwrap();

        assert_eq!(read(dir.path(), "old.txt"), "stable content here\n");
        assert!(!dir.path().join("new.txt").exists());
        assert!(repo.status().unwrap().files.is_empty());
    }

    #[test]
    fn only_the_chosen_files_are_discarded() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        commit_file(dir.path(), "b.txt", "b\n");
        fs::write(dir.path().join("a.txt"), "a changed\n").unwrap();
        fs::write(dir.path().join("b.txt"), "b changed\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.discard(&[&change(&repo, "a.txt")]).unwrap();

        assert_eq!(read(dir.path(), "a.txt"), "a\n");
        assert_eq!(read(dir.path(), "b.txt"), "b changed\n");
    }

    #[test]
    fn undo_puts_back_every_kind_of_discarded_change() {
        let dir = new_repo();
        commit_file(dir.path(), "mod.txt", "m\n");
        commit_file(dir.path(), "del.txt", "d\n");
        fs::write(dir.path().join("mod.txt"), "m changed\n").unwrap();
        fs::remove_file(dir.path().join("del.txt")).unwrap();
        fs::write(dir.path().join("new.txt"), "untracked\n").unwrap();
        fs::write(dir.path().join("added.txt"), "added\n").unwrap();
        git(dir.path(), &["add", "added.txt"]);
        let repo = Repo::open(dir.path()).unwrap();
        let before = repo.status().unwrap().files;
        let changes: Vec<_> = before.iter().collect();

        let backup = repo.discard(&changes).unwrap();
        assert!(
            repo.status().unwrap().files.is_empty(),
            "everything should be gone"
        );
        assert_eq!(backup.file_count(), 4);

        repo.undo_discard(&backup).unwrap();

        assert_eq!(read(dir.path(), "mod.txt"), "m changed\n");
        assert!(
            !dir.path().join("del.txt").exists(),
            "a deleted file stays deleted"
        );
        assert_eq!(read(dir.path(), "new.txt"), "untracked\n");
        assert_eq!(read(dir.path(), "added.txt"), "added\n");
        // The same set of files is changed again (as unstaged edits).
        let after: Vec<_> = repo
            .status()
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();
        let mut expected: Vec<_> = before.iter().map(|f| f.path.clone()).collect();
        expected.sort();
        assert_eq!(after, expected);
    }

    #[test]
    fn the_backup_survives_as_a_hidden_ref_with_the_old_content() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "original\n");
        fs::write(dir.path().join("a.txt"), "work I might want back\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        let backup = repo.discard(&[&change(&repo, "a.txt")]).unwrap();

        let saved = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["show", &format!("refs/ramus/discarded/{}:a.txt", backup.id)])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&saved.stdout),
            "work I might want back\n"
        );
        // It must not disturb the real index or history.
        assert!(repo.status().unwrap().files.is_empty());
        let log = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1);
    }

    #[test]
    fn a_failed_backup_leaves_the_working_tree_untouched() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("a.txt"), "my work\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let file = change(&repo, "a.txt");
        // Block the backup folder by making `.git/ramus` a file.
        fs::write(dir.path().join(".git/ramus"), "in the way").unwrap();

        assert!(repo.discard(&[&file]).is_err());
        assert_eq!(read(dir.path(), "a.txt"), "my work\n");
    }

    #[test]
    fn discarding_on_a_branch_with_no_commits_removes_staged_files() {
        let dir = new_repo();
        fs::write(dir.path().join("first.txt"), "hi\n").unwrap();
        git(dir.path(), &["add", "first.txt"]);
        let repo = Repo::open(dir.path()).unwrap();

        repo.discard(&[&change(&repo, "first.txt")]).unwrap();
        assert!(!dir.path().join("first.txt").exists());
    }

    #[test]
    fn conflicted_files_are_refused() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "base\n");
        git(dir.path(), &["switch", "-q", "-c", "other"]);
        commit_file(dir.path(), "a.txt", "other\n");
        git(dir.path(), &["switch", "-q", "main"]);
        commit_file(dir.path(), "a.txt", "main\n");
        let _ = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["merge", "other"])
            .output();
        let repo = Repo::open(dir.path()).unwrap();

        assert!(repo.discard(&[&change(&repo, "a.txt")]).is_err());
    }
}

mod amending {
    use super::*;

    fn subjects(dir: &Path) -> Vec<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["log", "--format=%s"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_last_commit_message_is_split_into_summary_and_description() {
        let dir = new_repo();
        let repo = Repo::open(dir.path()).unwrap();
        assert_eq!(repo.last_commit_message().unwrap(), None);

        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        git(dir.path(), &["add", "."]);
        repo.commit("Add a", "Because we need it.\n\nSecond paragraph.")
            .unwrap();

        assert_eq!(
            repo.last_commit_message().unwrap(),
            Some((
                "Add a".into(),
                "Because we need it.\n\nSecond paragraph.".into()
            ))
        );
    }

    #[test]
    fn amending_adds_staged_changes_to_the_last_commit_instead_of_making_a_new_one() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        commit_file(dir.path(), "b.txt", "b\n");
        fs::write(dir.path().join("c.txt"), "c\n").unwrap();
        git(dir.path(), &["add", "c.txt"]);
        let repo = Repo::open(dir.path()).unwrap();

        repo.amend("Add b and c", "").unwrap();

        assert_eq!(subjects(dir.path()), ["Add b and c", "add a.txt"]);
        let shown = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["show", "--name-only", "--format=", "HEAD"])
            .output()
            .unwrap();
        let shown = String::from_utf8_lossy(&shown.stdout);
        assert!(
            shown.contains("b.txt") && shown.contains("c.txt"),
            "{shown}"
        );
        assert!(repo.status().unwrap().files.is_empty());
    }

    #[test]
    fn amending_with_nothing_staged_just_rewrites_the_message() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();

        repo.amend("A better message", "With a description.")
            .unwrap();

        assert_eq!(subjects(dir.path()), ["A better message"]);
        assert_eq!(
            repo.last_commit_message().unwrap().unwrap().1,
            "With a description."
        );
    }

    #[test]
    fn amending_leaves_unstaged_work_alone() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::write(dir.path().join("a.txt"), "unstaged edit\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        repo.amend("Reworded", "").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "unstaged edit\n"
        );
        assert_eq!(repo.status().unwrap().files.len(), 1);
    }

    #[test]
    fn amending_a_branch_with_no_commits_is_an_error() {
        let dir = new_repo();
        let repo = Repo::open(dir.path()).unwrap();
        assert!(matches!(
            repo.amend("x", "").unwrap_err(),
            Error::Git { .. }
        ));
    }
}

mod discard_symlink_safety {
    use git_core::FileChange;

    use super::*;

    fn change(repo: &Repo, path: &str) -> FileChange {
        repo.status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == path)
            .unwrap()
    }

    #[test]
    fn undo_will_not_write_through_a_folder_that_became_a_symlink() {
        let outside = TempDir::new().unwrap();
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/new.txt"), "mine\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let backup = repo.discard(&[&change(&repo, "docs/new.txt")]).unwrap();

        // After the discard, `docs` is swapped for a symlink to somewhere else entirely.
        fs::remove_dir_all(dir.path().join("docs")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("docs")).unwrap();

        let result = repo.undo_discard(&backup);

        assert!(
            result.is_err(),
            "undo should refuse to go through the symlink"
        );
        assert!(
            fs::read_dir(outside.path()).unwrap().next().is_none(),
            "nothing may be written outside the repository"
        );
    }

    #[test]
    fn undo_will_not_delete_through_a_folder_that_became_a_symlink() {
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("x.txt"), "must survive\n").unwrap();
        let dir = new_repo();
        fs::create_dir_all(dir.path().join("d")).unwrap();
        commit_file(dir.path(), "d/x.txt", "tracked\n");
        fs::remove_file(dir.path().join("d/x.txt")).unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        // Discarding a deletion brings the file back; undoing that deletes it again.
        let backup = repo.discard(&[&change(&repo, "d/x.txt")]).unwrap();

        fs::remove_dir_all(dir.path().join("d")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("d")).unwrap();

        let _ = repo.undo_discard(&backup);

        assert_eq!(
            fs::read_to_string(outside.path().join("x.txt")).unwrap(),
            "must survive\n",
            "undo deleted a file outside the repository"
        );
    }

    #[test]
    fn discarding_will_not_move_a_file_from_behind_a_symlink() {
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.txt"), "not part of the repo\n").unwrap();
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/new.txt"), "mine\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let file = change(&repo, "docs/new.txt");
        // The status said docs/new.txt; by the time we act, docs points elsewhere.
        fs::remove_dir_all(dir.path().join("docs")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("docs")).unwrap();
        fs::write(
            outside.path().join("new.txt"),
            "outside file with the same name\n",
        )
        .unwrap();

        let result = repo.discard(&[&file]);

        assert!(result.is_err());
        assert!(
            outside.path().join("new.txt").exists(),
            "a file outside was moved away"
        );
    }

    #[test]
    fn an_ordinary_undo_with_nested_folders_still_works() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        fs::create_dir_all(dir.path().join("deep/er/still")).unwrap();
        fs::write(dir.path().join("deep/er/still/new.txt"), "keep\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let backup = repo
            .discard(&[&change(&repo, "deep/er/still/new.txt")])
            .unwrap();
        fs::remove_dir_all(dir.path().join("deep")).unwrap();

        repo.undo_discard(&backup).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("deep/er/still/new.txt")).unwrap(),
            "keep\n"
        );
    }
}

mod history {
    use git_core::{CommitInfo, DiffOptions};

    use super::*;

    fn log_all(repo: &Repo) -> Vec<CommitInfo> {
        repo.log(0, 1000).unwrap()
    }

    fn head_subject(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["log", "-1", "--format=%s"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn a_branch_with_no_commits_has_an_empty_log() {
        let dir = new_repo();
        assert!(
            Repo::open(dir.path())
                .unwrap()
                .log(0, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_log_is_newest_first_and_can_be_paged() {
        let dir = new_repo();
        for n in 1..=5 {
            commit_file(dir.path(), &format!("f{n}.txt"), "x\n");
        }
        let repo = Repo::open(dir.path()).unwrap();

        let all = log_all(&repo);
        let subjects: Vec<_> = all.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(
            subjects,
            [
                "add f5.txt",
                "add f4.txt",
                "add f3.txt",
                "add f2.txt",
                "add f1.txt"
            ]
        );

        let page_two = repo.log(2, 2).unwrap();
        assert_eq!(
            page_two
                .iter()
                .map(|c| c.subject.as_str())
                .collect::<Vec<_>>(),
            ["add f3.txt", "add f2.txt"]
        );
        assert_eq!(all[0].author, "Test");
        assert!(all[0].short_id.len() >= 7);
        assert!(all[4].parents.is_empty());
    }

    #[test]
    fn commits_not_on_the_upstream_are_flagged_as_unpushed() {
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
        let repo = Repo::open(dir.path()).unwrap();

        let unpushed = repo.unpushed_commits().unwrap();
        let log = log_all(&repo);
        assert_eq!(unpushed.len(), 1);
        assert!(unpushed.contains(&log[0].id));
        assert!(!unpushed.contains(&log[1].id));
    }

    #[test]
    fn with_no_upstream_nothing_is_called_unpushed() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        assert!(
            Repo::open(dir.path())
                .unwrap()
                .unpushed_commits()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_commit_lists_its_files_with_line_counts() {
        let dir = new_repo();
        commit_file(dir.path(), "keep.txt", "one\n");
        fs::write(dir.path().join("keep.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(dir.path().join("new.txt"), "n\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "change two files"]);
        let repo = Repo::open(dir.path()).unwrap();

        let commit = log_all(&repo).remove(0);
        let mut files = repo.commit_files(&commit).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(files.len(), 2);
        assert_eq!(
            (
                files[0].path.as_str(),
                files[0].additions,
                files[0].deletions
            ),
            ("keep.txt", Some(2), Some(0))
        );
        assert_eq!(
            (files[1].path.as_str(), files[1].additions),
            ("new.txt", Some(1))
        );
    }

    #[test]
    fn the_first_commit_can_be_inspected_too() {
        let dir = new_repo();
        commit_file(dir.path(), "first.txt", "hello\n");
        let repo = Repo::open(dir.path()).unwrap();
        let root = log_all(&repo).remove(0);

        let files = repo.commit_files(&root).unwrap();
        assert_eq!(files.len(), 1);
        let diff = repo
            .commit_diff(&root, &files[0], DiffOptions::default())
            .unwrap();
        assert_eq!((diff.additions(), diff.deletions()), (1, 0));
    }

    #[test]
    fn a_commit_diff_shows_only_what_that_commit_changed() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "1\n2\n3\n");
        fs::write(dir.path().join("a.txt"), "1\n2 changed\n3\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "tweak"]);
        fs::write(dir.path().join("a.txt"), "1\n2 changed\n3 later\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "later"]);
        let repo = Repo::open(dir.path()).unwrap();

        let log = log_all(&repo);
        let tweak = log.iter().find(|c| c.subject == "tweak").unwrap();
        let file = &repo.commit_files(tweak).unwrap()[0];
        let diff = repo
            .commit_diff(tweak, file, DiffOptions::default())
            .unwrap();

        let added: Vec<_> = diff.hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == git_core::DiffLineKind::Added)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(added, ["2 changed"]);
    }

    #[test]
    fn a_renamed_file_shows_its_old_path_and_diffs_as_a_rename() {
        let dir = new_repo();
        commit_file(dir.path(), "old.txt", "a\nb\nc\nd\ne\nf\n");
        git(dir.path(), &["mv", "old.txt", "new.txt"]);
        fs::write(dir.path().join("new.txt"), "a\nb\nc\nd\ne\nF\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "rename and edit"]);
        let repo = Repo::open(dir.path()).unwrap();

        let commit = log_all(&repo).remove(0);
        let files = repo.commit_files(&commit).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].original_path.as_deref(), Some("old.txt"));
        let diff = repo
            .commit_diff(&commit, &files[0], DiffOptions::default())
            .unwrap();
        assert_eq!((diff.additions(), diff.deletions()), (1, 1));
    }

    #[test]
    fn a_merge_commit_is_shown_against_its_first_parent() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(dir.path(), &["switch", "-q", "-c", "side"]);
        commit_file(dir.path(), "side.txt", "s\n");
        git(dir.path(), &["switch", "-q", "main"]);
        commit_file(dir.path(), "main.txt", "m\n");
        git(
            dir.path(),
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );
        let repo = Repo::open(dir.path()).unwrap();

        let merge = log_all(&repo).remove(0);
        assert!(merge.is_merge());
        let files = repo.commit_files(&merge).unwrap();
        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["side.txt"]
        );
    }

    #[test]
    fn reverting_a_commit_adds_a_commit_that_undoes_it() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        commit_file(dir.path(), "b.txt", "b\n");
        let repo = Repo::open(dir.path()).unwrap();
        let target = log_all(&repo).remove(0);

        repo.revert(&target).unwrap();

        assert!(!dir.path().join("b.txt").exists());
        assert!(head_subject(dir.path()).starts_with("Revert"));
        assert_eq!(log_all(&repo).len(), 3);
    }

    #[test]
    fn a_conflicting_revert_stays_in_progress_with_the_conflict_in_place() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");
        fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "to two"]);
        fs::write(dir.path().join("a.txt"), "three\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "to three"]);
        let repo = Repo::open(dir.path()).unwrap();
        let to_two = log_all(&repo)
            .into_iter()
            .find(|c| c.subject == "to two")
            .unwrap();

        let error = repo.revert(&to_two).unwrap_err();

        assert!(
            matches!(&error, Error::Conflicts(files) if files == &["a.txt"]),
            "{error}"
        );
        assert_eq!(
            repo.in_progress().unwrap().map(|p| p.kind),
            Some(git_core::OperationKind::Revert)
        );
        assert!(
            fs::read_to_string(dir.path().join("a.txt"))
                .unwrap()
                .contains("<<<<<<<")
        );
        repo.abort_operation().unwrap();
        assert!(repo.in_progress().unwrap().is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "three\n"
        );
        assert!(repo.status().unwrap().files.is_empty());
    }

    #[test]
    fn cherry_picking_brings_one_commit_across_branches() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(dir.path(), &["switch", "-q", "-c", "other"]);
        commit_file(dir.path(), "picked.txt", "p\n");
        commit_file(dir.path(), "skipped.txt", "s\n");
        git(dir.path(), &["switch", "-q", "main"]);
        let repo = Repo::open(dir.path()).unwrap();
        git(dir.path(), &["switch", "-q", "other"]);
        let picked = log_all(&repo)
            .into_iter()
            .find(|c| c.subject == "add picked.txt")
            .unwrap();
        git(dir.path(), &["switch", "-q", "main"]);

        repo.cherry_pick(&picked).unwrap();

        assert!(dir.path().join("picked.txt").exists());
        assert!(!dir.path().join("skipped.txt").exists());
        assert_eq!(head_subject(dir.path()), "add picked.txt");
    }

    #[test]
    fn a_conflicting_cherry_pick_stays_in_progress_and_can_be_aborted() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "base\n");
        git(dir.path(), &["switch", "-q", "-c", "other"]);
        fs::write(dir.path().join("a.txt"), "other side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "other edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "main side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "main edit"]);
        let repo = Repo::open(dir.path()).unwrap();
        git(dir.path(), &["switch", "-q", "other"]);
        let pick = log_all(&repo).remove(0);
        git(dir.path(), &["switch", "-q", "main"]);

        assert!(matches!(repo.cherry_pick(&pick), Err(Error::Conflicts(_))));
        assert_eq!(
            repo.in_progress().unwrap().map(|p| p.kind),
            Some(git_core::OperationKind::CherryPick)
        );

        repo.abort_operation().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "main side\n"
        );
        assert!(repo.status().unwrap().files.is_empty());
        assert!(!dir.path().join(".git/CHERRY_PICK_HEAD").exists());
    }

    #[test]
    fn merging_a_branch_brings_in_its_commits() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        commit_file(dir.path(), "feature.txt", "f\n");
        git(dir.path(), &["switch", "-q", "main"]);
        commit_file(dir.path(), "main.txt", "m\n");
        let repo = Repo::open(dir.path()).unwrap();

        repo.merge("feature").unwrap();

        assert!(dir.path().join("feature.txt").exists() && dir.path().join("main.txt").exists());
        assert!(log_all(&repo)[0].is_merge());
    }

    #[test]
    fn a_conflicting_merge_stays_in_progress_and_names_the_branch() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "base\n");
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("a.txt"), "feature side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "feature edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "main side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "main edit"]);
        let repo = Repo::open(dir.path()).unwrap();
        let before = log_all(&repo)[0].id.clone();

        let error = repo.merge("feature").unwrap_err();

        assert!(
            matches!(&error, Error::Conflicts(files) if files == &["a.txt"]),
            "{error}"
        );
        let progress = repo.in_progress().unwrap().unwrap();
        assert_eq!(progress.kind, git_core::OperationKind::Merge);
        assert_eq!(progress.merging.as_deref(), Some("feature"));

        repo.abort_operation().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "main side\n"
        );
        assert!(repo.status().unwrap().files.is_empty());
        assert_eq!(log_all(&repo)[0].id, before);
        assert!(!dir.path().join(".git/MERGE_HEAD").exists());
    }

    #[test]
    fn a_commit_message_full_of_separator_bytes_cannot_break_the_log() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        // Record and unit separators are legal in a commit message.
        let message = dir.path().join("msg");
        fs::write(
            &message,
            "sneaky subject\n\nbody\x1e with \x1f and \x1e\x1e more\n",
        )
        .unwrap();
        git(
            dir.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-F",
                message.to_str().unwrap(),
            ],
        );
        commit_file(dir.path(), "b.txt", "b\n");
        let repo = Repo::open(dir.path()).unwrap();

        let commits = repo.log(0, 50).expect("the log must still be readable");

        assert_eq!(commits.len(), 3);
        let sneaky = commits
            .iter()
            .find(|c| c.subject == "sneaky subject")
            .unwrap();
        assert!(sneaky.body.contains("\x1e with \x1f"), "{:?}", sneaky.body);
        assert!(
            commits
                .iter()
                .all(|c| c.id.len() >= 40 && c.id.chars().all(|ch| ch.is_ascii_hexdigit()))
        );
    }

    #[test]
    fn merge_refuses_a_name_that_looks_like_an_option() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let repo = Repo::open(dir.path()).unwrap();
        assert!(matches!(
            repo.merge("--abort"),
            Err(Error::InvalidBranchName(_))
        ));
        assert!(matches!(repo.merge(""), Err(Error::InvalidBranchName(_))));
    }
}

mod line_staging {
    use std::collections::BTreeSet;

    use git_core::{DiffLineKind, DiffOptions};

    use super::*;

    fn ten_lines() -> String {
        (1..=10).map(|n| format!("l{n}\n")).collect()
    }

    /// `l3` replaced, a line added after `l6`, `l8` deleted: three separate changes that
    /// still share one hunk.
    fn edited() -> String {
        ten_lines()
            .replace("l3\n", "l3 NEW\n")
            .replace("l6\n", "l6\nadded after 6\n")
            .replace("l8\n", "")
    }

    fn setup() -> (TempDir, Repo) {
        let dir = new_repo();
        commit_file(dir.path(), "f.txt", &ten_lines());
        fs::write(dir.path().join("f.txt"), edited()).unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    fn change(repo: &Repo) -> git_core::FileChange {
        repo.status()
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == "f.txt")
            .unwrap()
    }

    fn unstaged_hunk(repo: &Repo) -> git_core::DiffHunk {
        let diffs = repo
            .file_diffs(&change(repo), DiffOptions::default())
            .unwrap();
        assert_eq!(
            diffs.unstaged.hunks.len(),
            1,
            "the three changes should share a hunk"
        );
        diffs.unstaged.hunks[0].clone()
    }

    fn staged_hunk(repo: &Repo) -> git_core::DiffHunk {
        let diffs = repo
            .file_diffs(&change(repo), DiffOptions::default())
            .unwrap();
        diffs.staged.hunks[0].clone()
    }

    /// The index of the first line of `hunk` with this kind and text.
    fn find(hunk: &git_core::DiffHunk, kind: DiffLineKind, text: &str) -> usize {
        hunk.lines
            .iter()
            .position(|l| l.kind == kind && l.text == text)
            .unwrap_or_else(|| panic!("no {kind:?} line {text:?}"))
    }

    fn only(index: usize) -> BTreeSet<usize> {
        BTreeSet::from([index])
    }

    fn index_content(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show", ":f.txt"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn staging_only_an_added_line_leaves_the_removal_it_replaces_unstaged() {
        let (dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);

        repo.stage_lines(
            "f.txt",
            &hunk,
            &only(find(&hunk, DiffLineKind::Added, "l3 NEW")),
        )
        .unwrap();

        let staged = index_content(dir.path());
        let staged = lines(&staged);
        assert!(
            staged.contains(&"l3"),
            "the old line is still there: {staged:?}"
        );
        assert!(staged.contains(&"l3 NEW"));
        assert!(
            !staged.contains(&"added after 6"),
            "nothing else was staged"
        );
        assert!(staged.contains(&"l8"));
    }

    #[test]
    fn staging_only_a_removal_deletes_just_that_line_from_the_index() {
        let (dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);

        repo.stage_lines(
            "f.txt",
            &hunk,
            &only(find(&hunk, DiffLineKind::Removed, "l8")),
        )
        .unwrap();

        let staged = index_content(dir.path());
        let staged = lines(&staged);
        assert!(!staged.contains(&"l8"));
        assert!(staged.contains(&"l3") && !staged.contains(&"l3 NEW"));
        assert!(!staged.contains(&"added after 6"));
    }

    #[test]
    fn staging_a_replacement_pair_stages_just_that_replacement() {
        let (dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);
        let pair = BTreeSet::from([
            find(&hunk, DiffLineKind::Removed, "l3"),
            find(&hunk, DiffLineKind::Added, "l3 NEW"),
        ]);

        repo.stage_lines("f.txt", &hunk, &pair).unwrap();

        let staged = index_content(dir.path());
        let staged = lines(&staged);
        assert!(staged.contains(&"l3 NEW") && !staged.contains(&"l3"));
        assert!(!staged.contains(&"added after 6") && staged.contains(&"l8"));
        // The other two changes are still waiting in the working tree.
        let rest = repo
            .file_diffs(&change(&repo), DiffOptions::default())
            .unwrap()
            .unstaged;
        assert_eq!((rest.additions(), rest.deletions()), (1, 1));
    }

    #[test]
    fn the_changes_not_chosen_stay_exactly_as_they_were_in_the_working_tree() {
        let (dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);

        repo.stage_lines(
            "f.txt",
            &hunk,
            &only(find(&hunk, DiffLineKind::Added, "added after 6")),
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            edited()
        );
    }

    #[test]
    fn selecting_every_changed_line_is_the_same_as_staging_the_whole_hunk() {
        let (dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);
        let all: BTreeSet<usize> = hunk
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind != DiffLineKind::Context)
            .map(|(ix, _)| ix)
            .collect();

        repo.stage_lines("f.txt", &hunk, &all).unwrap();

        assert_eq!(index_content(dir.path()), edited());
        assert!(change(&repo).is_fully_staged());
    }

    #[test]
    fn staging_lines_one_at_a_time_builds_up_to_the_whole_change() {
        let (dir, repo) = setup();
        for text in ["l3 NEW", "added after 6"] {
            let hunk = unstaged_hunk(&repo);
            repo.stage_lines(
                "f.txt",
                &hunk,
                &only(find(&hunk, DiffLineKind::Added, text)),
            )
            .unwrap();
        }
        for text in ["l3", "l8"] {
            let hunk = unstaged_hunk(&repo);
            repo.stage_lines(
                "f.txt",
                &hunk,
                &only(find(&hunk, DiffLineKind::Removed, text)),
            )
            .unwrap();
        }
        assert_eq!(index_content(dir.path()), edited());
    }

    #[test]
    fn unstaging_an_added_line_keeps_the_other_staged_changes() {
        let (dir, repo) = setup();
        repo.stage(&["f.txt"]).unwrap();
        let hunk = staged_hunk(&repo);

        repo.unstage_lines(
            "f.txt",
            &hunk,
            &only(find(&hunk, DiffLineKind::Added, "added after 6")),
        )
        .unwrap();

        let staged = index_content(dir.path());
        let staged = lines(&staged);
        assert!(
            !staged.contains(&"added after 6"),
            "that line is out of the index"
        );
        assert!(
            staged.contains(&"l3 NEW") && !staged.contains(&"l3"),
            "the replacement is still staged"
        );
        assert!(!staged.contains(&"l8"), "the deletion is still staged");
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            edited()
        );
    }

    #[test]
    fn unstaging_a_removal_puts_that_line_back_in_the_index() {
        let (dir, repo) = setup();
        repo.stage(&["f.txt"]).unwrap();
        let hunk = staged_hunk(&repo);

        repo.unstage_lines(
            "f.txt",
            &hunk,
            &only(find(&hunk, DiffLineKind::Removed, "l8")),
        )
        .unwrap();

        let staged = index_content(dir.path());
        let staged = lines(&staged);
        assert!(staged.contains(&"l8"));
        assert!(staged.contains(&"l3 NEW") && staged.contains(&"added after 6"));
    }

    #[test]
    fn a_commit_after_staging_some_lines_contains_only_those_lines() {
        let (dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);
        repo.stage_lines(
            "f.txt",
            &hunk,
            &only(find(&hunk, DiffLineKind::Added, "added after 6")),
        )
        .unwrap();

        repo.commit("Just the added line", "").unwrap();

        let shown = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["show", "HEAD", "--format=", "-U0"])
            .output()
            .unwrap();
        let shown = String::from_utf8_lossy(&shown.stdout);
        assert!(shown.contains("+added after 6"), "{shown}");
        assert!(
            !shown.contains("l3 NEW") && !shown.contains("-l8"),
            "{shown}"
        );
    }

    #[test]
    fn selecting_only_context_or_nothing_is_an_error() {
        let (_dir, repo) = setup();
        let hunk = unstaged_hunk(&repo);
        let context = hunk
            .lines
            .iter()
            .position(|l| l.kind == DiffLineKind::Context)
            .unwrap();

        assert!(repo.stage_lines("f.txt", &hunk, &BTreeSet::new()).is_err());
        assert!(repo.stage_lines("f.txt", &hunk, &only(context)).is_err());
    }

    #[test]
    fn a_partial_change_at_the_end_of_a_file_without_a_newline_is_refused_not_corrupted() {
        let dir = new_repo();
        commit_file(dir.path(), "f.txt", "a\nb");
        fs::write(dir.path().join("f.txt"), "a\nb2").unwrap();
        let repo = Repo::open(dir.path()).unwrap();
        let hunk = repo
            .file_diffs(&change(&repo), DiffOptions::default())
            .unwrap()
            .unstaged
            .hunks[0]
            .clone();
        let before = index_content(dir.path());

        let error = repo
            .stage_lines(
                "f.txt",
                &hunk,
                &only(find(&hunk, DiffLineKind::Added, "b2")),
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("no newline"), "{error}");
        assert_eq!(
            index_content(dir.path()),
            before,
            "the index must be untouched"
        );
        // Choosing both lines is the whole change, which is fine.
        let both = BTreeSet::from([
            find(&hunk, DiffLineKind::Removed, "b"),
            find(&hunk, DiffLineKind::Added, "b2"),
        ]);
        repo.stage_lines("f.txt", &hunk, &both).unwrap();
        assert_eq!(index_content(dir.path()), "a\nb2");
    }

    #[test]
    fn file_names_with_spaces_and_quotes_work_for_partial_staging() {
        for name in ["my notes.txt", "q\"uote.txt"] {
            let dir = new_repo();
            commit_file(dir.path(), name, &ten_lines());
            fs::write(dir.path().join(name), edited()).unwrap();
            let repo = Repo::open(dir.path()).unwrap();
            let change = repo
                .status()
                .unwrap()
                .files
                .into_iter()
                .find(|f| f.path == name)
                .unwrap();
            let hunk = repo
                .file_diffs(&change, DiffOptions::default())
                .unwrap()
                .unstaged
                .hunks[0]
                .clone();

            repo.stage_lines(
                name,
                &hunk,
                &only(find(&hunk, DiffLineKind::Added, "added after 6")),
            )
            .unwrap_or_else(|e| panic!("{name:?}: {e}"));

            let staged = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["show", &format!(":{name}")])
                .output()
                .unwrap();
            assert!(
                String::from_utf8_lossy(&staged.stdout).contains("added after 6"),
                "{name:?}"
            );
        }
    }
}

mod untrusted_signature_verification {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A repo whose latest commit carries a (fake) signature header, with the repo's own
    /// config telling git to verify signatures using a program of its choosing.
    fn repo_that_verifies_signatures_with_a_hostile_program()
    -> (TempDir, TempDir, std::path::PathBuf) {
        let outside = TempDir::new().unwrap();
        let marker = outside.path().join("PWNED");
        let script = outside.path().join("evil-gpg.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        let out = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            String::from_utf8(o.stdout).unwrap().trim().to_owned()
        };
        let tree = out(&["rev-parse", "HEAD^{tree}"]);
        let parent = out(&["rev-parse", "HEAD"]);
        let raw = format!(
            "tree {tree}\nparent {parent}\nauthor A <a@x.io> 1700000000 +0000\n\
             committer A <a@x.io> 1700000000 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n \n fake\n -----END PGP SIGNATURE-----\n\nsigned-looking\n"
        );
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), raw.as_bytes()).unwrap();
        let id = String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        git(dir.path(), &["update-ref", "HEAD", &id]);

        git(dir.path(), &["config", "log.showSignature", "true"]);
        git(
            dir.path(),
            &["config", "gpg.program", script.to_str().unwrap()],
        );
        (dir, outside, marker)
    }

    #[test]
    fn the_harness_detects_gpg_being_run_by_plain_git_log() {
        let (dir, _outside, marker) = repo_that_verifies_signatures_with_a_hostile_program();
        let _ = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["log", "-1"])
            .output()
            .unwrap();
        assert!(
            marker.exists(),
            "plain `git log` should run the configured gpg program"
        );
    }

    #[test]
    fn reading_the_log_never_runs_a_repo_configured_signature_program() {
        let (dir, _outside, marker) = repo_that_verifies_signatures_with_a_hostile_program();

        let commits = Repo::open(dir.path()).unwrap().log(0, 10).unwrap();

        assert_eq!(commits.len(), 2, "the log should still be readable");
        assert!(!marker.exists(), "the log ran the repository's gpg program");
    }
}

mod cloning {
    use std::time::{Duration, Instant};

    use git_core::CancelToken;

    use super::*;

    fn no_progress(_: git_core::Progress) {}

    /// A repository with one commit to clone from.
    fn source() -> TempDir {
        let dir = new_repo();
        commit_file(dir.path(), "hello.txt", "hi\n");
        dir
    }

    fn clone_into(url: &str, destination: &Path) -> Result<Repo, Error> {
        Repo::clone_from(url, destination, &CancelToken::new(), &mut no_progress)
    }

    #[test]
    fn clones_into_a_new_folder_and_opens_it() {
        let from = source();
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("copy");

        let repo = clone_into(from.path().to_str().unwrap(), &destination).unwrap();

        assert_eq!(
            repo.root().canonicalize().unwrap(),
            destination.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_to_string(destination.join("hello.txt")).unwrap(),
            "hi\n"
        );
        assert!(repo.status().unwrap().branch.upstream.is_some());
    }

    #[test]
    fn creates_missing_parent_folders() {
        let from = source();
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("a/b/copy");
        clone_into(from.path().to_str().unwrap(), &destination).unwrap();
        assert!(destination.join("hello.txt").exists());
    }

    #[test]
    fn an_existing_empty_folder_is_fine() {
        let from = source();
        let destination = TempDir::new().unwrap();
        clone_into(from.path().to_str().unwrap(), destination.path()).unwrap();
        assert!(destination.path().join("hello.txt").exists());
    }

    #[test]
    fn a_folder_with_files_in_it_is_refused_and_left_alone() {
        let from = source();
        let destination = TempDir::new().unwrap();
        fs::write(destination.path().join("mine.txt"), "mine\n").unwrap();

        let error = clone_into(from.path().to_str().unwrap(), destination.path()).unwrap_err();

        assert!(matches!(error, Error::DestinationNotEmpty(_)), "{error}");
        assert_eq!(
            fs::read_to_string(destination.path().join("mine.txt")).unwrap(),
            "mine\n"
        );
        assert!(!destination.path().join("hello.txt").exists());
    }

    #[test]
    fn a_file_in_the_way_is_refused() {
        let from = source();
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("file");
        fs::write(&destination, "x").unwrap();
        let error = clone_into(from.path().to_str().unwrap(), &destination).unwrap_err();
        assert!(matches!(error, Error::DestinationNotEmpty(_)), "{error}");
    }

    #[test]
    fn a_failed_clone_leaves_no_folder_behind() {
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("copy");
        let missing = parent.path().join("no-such-repo");
        assert!(clone_into(missing.to_str().unwrap(), &destination).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn an_empty_url_is_rejected_without_running_git() {
        let parent = TempDir::new().unwrap();
        let error = clone_into("   ", &parent.path().join("copy")).unwrap_err();
        assert!(matches!(error, Error::InvalidUrl(_)), "{error}");
    }

    #[test]
    fn a_url_that_looks_like_an_option_is_not_one() {
        let parent = TempDir::new().unwrap();
        let marker = parent.path().join("ran");
        let url = format!("--upload-pack=touch {}", marker.display());
        // An existing, empty destination is what an option-shaped URL would end up
        // treating as the repository to clone from.
        let destination = parent.path().join("copy");
        fs::create_dir(&destination).unwrap();
        let error = clone_into(&url, &destination).unwrap_err();
        assert!(!marker.exists(), "git took the URL as an option");
        // Git names what it took for the repository; it must be the URL, not our folder.
        assert!(error.to_string().contains("--upload-pack="), "{error}");
    }

    #[test]
    fn the_ext_transport_cannot_run_commands() {
        let parent = TempDir::new().unwrap();
        let marker = parent.path().join("ran");
        let url = format!("ext::sh -c touch% {}", marker.display());
        let result = clone_into(&url, &parent.path().join("copy"));
        assert!(result.is_err());
        assert!(!marker.exists(), "the ext transport ran a command");
    }

    #[test]
    fn cancelling_stops_git_and_removes_what_it_started() {
        // The shared setup points ssh at a script that never answers, which keeps git busy
        // until it is killed.
        isolate_from_host_git_config();
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("copy");
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            canceller.cancel();
        });

        let started = Instant::now();
        let result = Repo::clone_from(
            "ssh://example.invalid/repo",
            &destination,
            &cancel,
            &mut no_progress,
        );

        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!destination.exists());
    }
}

mod merge_pull {
    use git_core::{CancelToken, Progress};

    use super::*;

    fn no_progress(_: Progress) {}

    fn clone_of(remote: &Path) -> TempDir {
        isolate_from_host_git_config();
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["clone", "-q", remote.to_str().unwrap(), "."]);
        dir
    }

    /// A bare remote holding `shared.txt`, and two clones of it.
    fn remote_with_two_clones() -> (TempDir, TempDir, TempDir) {
        isolate_from_host_git_config();
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let seed = new_repo();
        commit_file(seed.path(), "shared.txt", "one\ntwo\nthree\n");
        git(
            seed.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(seed.path(), &["push", "-q", "origin", "main"]);
        let (a, b) = (clone_of(remote.path()), clone_of(remote.path()));
        (remote, a, b)
    }

    fn parents_of_head(dir: &Path) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-list", "--parents", "-n1", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .count()
            - 1
    }

    #[test]
    fn diverged_branches_are_joined_by_a_merge_commit() {
        let (_remote, a, b) = remote_with_two_clones();
        commit_file(b.path(), "from-b.txt", "b\n");
        git(b.path(), &["push", "-q", "origin", "main"]);
        commit_file(a.path(), "from-a.txt", "a\n");
        let repo = Repo::open(a.path()).unwrap();

        // A plain pull refuses, as it should.
        assert!(repo.pull(&CancelToken::new(), &mut no_progress).is_err());

        repo.pull_merge(&CancelToken::new(), &mut no_progress)
            .unwrap();

        assert!(a.path().join("from-a.txt").exists());
        assert!(a.path().join("from-b.txt").exists());
        assert_eq!(parents_of_head(a.path()), 2);
        assert_eq!(repo.status().unwrap().branch.behind, 0);
    }

    #[test]
    fn a_conflict_stays_in_progress_for_the_user_to_resolve() {
        let (_remote, a, b) = remote_with_two_clones();
        fs::write(b.path().join("shared.txt"), "one\nB\nthree\n").unwrap();
        git(b.path(), &["commit", "-qam", "b edit"]);
        git(b.path(), &["push", "-q", "origin", "main"]);
        fs::write(a.path().join("shared.txt"), "one\nA\nthree\n").unwrap();
        git(a.path(), &["commit", "-qam", "a edit"]);
        let repo = Repo::open(a.path()).unwrap();

        let error = repo
            .pull_merge(&CancelToken::new(), &mut no_progress)
            .unwrap_err();

        assert!(
            matches!(&error, Error::Conflicts(f) if f == &["shared.txt"]),
            "{error}"
        );
        assert!(repo.in_progress().unwrap().is_some());
        repo.abort_operation().unwrap();
        assert_eq!(
            fs::read_to_string(a.path().join("shared.txt")).unwrap(),
            "one\nA\nthree\n"
        );
        assert!(repo.status().unwrap().files.is_empty());
        assert_eq!(parents_of_head(a.path()), 1);
    }

    #[test]
    fn it_is_a_plain_fast_forward_when_nothing_diverged() {
        let (_remote, a, b) = remote_with_two_clones();
        commit_file(b.path(), "from-b.txt", "b\n");
        git(b.path(), &["push", "-q", "origin", "main"]);
        let repo = Repo::open(a.path()).unwrap();
        repo.pull_merge(&CancelToken::new(), &mut no_progress)
            .unwrap();
        assert!(a.path().join("from-b.txt").exists());
        assert_eq!(parents_of_head(a.path()), 1);
    }
}

mod failure_kinds {
    use git_core::FailureKind;

    #[test]
    fn recognises_the_common_ways_a_remote_says_no() {
        let cases = [
            (
                "git push failed:  ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs",
                FailureKind::Rejected,
            ),
            (
                "! [rejected] main -> main (non-fast-forward)",
                FailureKind::Rejected,
            ),
            (
                "remote: HTTP Basic: Access denied\nfatal: Authentication failed for 'https://x/y.git/'",
                FailureKind::Authentication,
            ),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                FailureKind::Authentication,
            ),
            (
                "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.",
                FailureKind::Authentication,
            ),
            (
                "fatal: unable to access 'https://x/': Could not resolve host: x",
                FailureKind::Other,
            ),
            ("", FailureKind::Other),
        ];
        for (message, expected) in cases {
            assert_eq!(FailureKind::of(message), expected, "{message}");
        }
    }
}

mod remotes {
    use git_core::{CancelToken, Progress};

    use super::*;

    fn no_progress(_: Progress) {}

    #[test]
    fn a_repo_with_no_remote_can_be_given_origin() {
        let repo_dir = new_repo();
        let repo = Repo::open(repo_dir.path()).unwrap();
        assert!(repo.remotes().unwrap().is_empty());

        repo.add_origin("https://example.com/acme/thing.git")
            .unwrap();

        assert_eq!(repo.remotes().unwrap(), ["origin"]);
        let url = Command::new("git")
            .arg("-C")
            .arg(repo_dir.path())
            .args(["remote", "get-url", "origin"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&url.stdout).trim(),
            "https://example.com/acme/thing.git"
        );
    }

    #[test]
    fn it_refuses_when_a_remote_already_exists_and_changes_nothing() {
        let repo_dir = new_repo();
        let repo = Repo::open(repo_dir.path()).unwrap();
        repo.add_origin("https://example.com/first.git").unwrap();
        assert!(repo.add_origin("https://example.com/second.git").is_err());
        let url = Command::new("git")
            .arg("-C")
            .arg(repo_dir.path())
            .args(["remote", "get-url", "origin"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&url.stdout).trim(),
            "https://example.com/first.git"
        );
    }

    #[test]
    fn blank_and_option_shaped_addresses_are_rejected() {
        let repo_dir = new_repo();
        let repo = Repo::open(repo_dir.path()).unwrap();
        for bad in ["", "   ", "--mirror=push", "-f"] {
            assert!(
                matches!(repo.add_origin(bad), Err(Error::InvalidUrl(_))),
                "{bad:?}"
            );
        }
        assert!(repo.remotes().unwrap().is_empty());
    }

    #[test]
    fn a_remote_that_names_the_ext_transport_cannot_run_a_command_on_fetch() {
        let repo_dir = new_repo();
        commit_file(repo_dir.path(), "a.txt", "a\n");
        let marker = repo_dir.path().join("ran");
        git(
            repo_dir.path(),
            &[
                "remote",
                "add",
                "origin",
                &format!("ext::sh -c touch% {}", marker.display()),
            ],
        );
        // The repository's own config switching the transport on must not be enough.
        git(repo_dir.path(), &["config", "protocol.ext.allow", "always"]);
        let repo = Repo::open(repo_dir.path()).unwrap();
        let result = repo.fetch(&CancelToken::new(), &mut no_progress);
        assert!(result.is_err());
        assert!(!marker.exists(), "fetch ran the ext transport");
    }
}

mod stashing {
    use git_core::DiffOptions;

    use super::*;

    /// A repo with `a.txt` committed, then edited, plus an untracked `new.txt`.
    fn dirty_repo() -> TempDir {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");
        fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
        dir
    }

    fn is_clean(repo: &Repo) -> bool {
        repo.status().unwrap().files.is_empty()
    }

    #[test]
    fn stashing_sets_aside_tracked_and_untracked_changes() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();

        repo.stash_push("WIP theme tokens").unwrap();

        assert!(is_clean(&repo));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(!dir.path().join("new.txt").exists());
        let stashes = repo.stashes().unwrap();
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].index, 0);
        assert_eq!(stashes[0].message, "WIP theme tokens");
        assert_eq!(stashes[0].branch, "main");
    }

    #[test]
    fn without_a_message_the_default_one_is_made_readable() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("").unwrap();
        let stash = &repo.stashes().unwrap()[0];
        assert_eq!(stash.branch, "main");
        assert!(!stash.message.is_empty());
        assert!(!stash.message.starts_with("WIP on"), "{}", stash.message);
    }

    #[test]
    fn nothing_to_stash_is_an_error_not_an_empty_stash() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");
        let repo = Repo::open(dir.path()).unwrap();
        assert!(repo.stash_push("x").is_err());
        assert!(repo.stashes().unwrap().is_empty());
    }

    #[test]
    fn the_newest_stash_is_first() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("first").unwrap();
        fs::write(dir.path().join("a.txt"), "again\n").unwrap();
        repo.stash_push("second").unwrap();
        let stashes = repo.stashes().unwrap();
        let names: Vec<_> = stashes
            .iter()
            .map(|s| (s.index, s.message.as_str()))
            .collect();
        assert_eq!(names, [(0, "second"), (1, "first")]);
    }

    #[test]
    fn apply_keeps_the_stash_and_pop_removes_it() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("work").unwrap();

        let stash = repo.stashes().unwrap().remove(0);
        repo.stash_apply(&stash).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert!(dir.path().join("new.txt").exists());
        assert_eq!(repo.stashes().unwrap().len(), 1);

        git(dir.path(), &["checkout", "--", "a.txt"]);
        fs::remove_file(dir.path().join("new.txt")).unwrap();
        let stash = repo.stashes().unwrap().remove(0);
        repo.stash_pop(&stash).unwrap();
        assert!(repo.stashes().unwrap().is_empty());
        assert!(dir.path().join("new.txt").exists());
    }

    #[test]
    fn drop_discards_only_that_stash() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("keep").unwrap();
        fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        repo.stash_push("lose").unwrap();
        let stashes = repo.stashes().unwrap();
        repo.stash_drop(&stashes[0]).unwrap();
        let left = repo.stashes().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, "keep");
    }

    #[test]
    fn a_stale_entry_is_refused_instead_of_hitting_a_different_stash() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("old").unwrap();
        let shown = repo.stashes().unwrap().remove(0);
        // Someone stashes again (a terminal, say) after the list was drawn.
        fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        repo.stash_push("newer").unwrap();

        assert!(repo.stash_drop(&shown).is_err());
        assert!(repo.stash_apply(&shown).is_err());
        assert!(repo.stash_pop(&shown).is_err());
        assert_eq!(repo.stashes().unwrap().len(), 2);
        assert!(is_clean(&repo));
    }

    #[test]
    fn popping_over_clashing_local_changes_fails_and_keeps_both() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("work").unwrap();
        fs::write(dir.path().join("a.txt"), "local edit\n").unwrap();
        let stash = repo.stashes().unwrap().remove(0);

        assert!(repo.stash_pop(&stash).is_err());

        assert_eq!(repo.stashes().unwrap().len(), 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "local edit\n"
        );
    }

    #[test]
    fn a_stash_lists_its_files_and_diffs_including_untracked_ones() {
        let dir = dirty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        repo.stash_push("work").unwrap();
        let stash = repo.stashes().unwrap().remove(0);

        let files = repo.stash_files(&stash).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.file.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "new.txt"]);

        let tracked = repo
            .stash_diff(&stash, &files[0], DiffOptions::default())
            .unwrap();
        assert!(tracked.hunks[0].lines.iter().any(|l| l.text == "two"));
        let untracked = repo
            .stash_diff(&stash, &files[1], DiffOptions::default())
            .unwrap();
        assert!(untracked.hunks[0].lines.iter().any(|l| l.text == "fresh"));
    }
}

mod stash_subjects {
    use git_core::parse_stash_subject;

    #[test]
    fn splits_the_branch_from_the_message() {
        assert_eq!(
            parse_stash_subject("On main: WIP theme tokens"),
            ("main".to_owned(), "WIP theme tokens".to_owned())
        );
        assert_eq!(
            parse_stash_subject("On feat/x: a: b"),
            ("feat/x".to_owned(), "a: b".to_owned())
        );
        assert_eq!(
            parse_stash_subject("WIP on main: 1a2b3c4 Fix the thing"),
            ("main".to_owned(), "Fix the thing".to_owned())
        );
    }

    #[test]
    fn anything_else_is_kept_whole() {
        assert_eq!(
            parse_stash_subject("something odd"),
            (String::new(), "something odd".to_owned())
        );
    }
}

mod resolving_conflicts {
    use git_core::{Choice, ConflictKind, Side, resolve_region};

    use super::*;

    /// `main` and `feature` edit line two of `f.txt` differently; `main` is checked out.
    fn clashing_repo() -> (TempDir, Repo) {
        let dir = new_repo();
        commit_file(dir.path(), "f.txt", "one\ntwo\nthree\n");
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("f.txt"), "one\nFEATURE\nthree\n").unwrap();
        git(dir.path(), &["commit", "-qam", "feature edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("f.txt"), "one\nMAIN\nthree\n").unwrap();
        git(dir.path(), &["commit", "-qam", "main edit"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    fn head_parents(dir: &Path) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-list", "--parents", "-n1", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .count()
            - 1
    }

    #[test]
    fn nothing_is_in_progress_in_an_ordinary_repo() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "a\n");
        assert!(
            Repo::open(dir.path())
                .unwrap()
                .in_progress()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_conflicted_file_is_read_with_its_markers_and_classified() {
        let (_dir, repo) = clashing_repo();
        assert!(repo.merge("feature").is_err());

        assert_eq!(
            repo.conflict_kind("f.txt").unwrap(),
            ConflictKind::BothChanged
        );
        let text = repo.conflict_text("f.txt").unwrap().unwrap();
        let regions = git_core::conflict_regions(&text);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].ours, ["MAIN\n"]);
        assert_eq!(regions[0].theirs, ["FEATURE\n"]);
    }

    #[test]
    fn resolving_by_text_then_continuing_commits_a_merge() {
        let (dir, repo) = clashing_repo();
        assert!(repo.merge("feature").is_err());
        let text = repo.conflict_text("f.txt").unwrap().unwrap();
        let both = resolve_region(&text, 0, Choice::Both);

        repo.resolve_with_text("f.txt", &both).unwrap();
        assert!(repo.conflicted_paths().unwrap().is_empty());
        repo.continue_operation().unwrap();

        assert!(repo.in_progress().unwrap().is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one\nMAIN\nFEATURE\nthree\n"
        );
        assert_eq!(head_parents(dir.path()), 2, "a merge commit");
        assert!(repo.status().unwrap().files.is_empty());
    }

    #[test]
    fn taking_a_whole_side_resolves_the_file() {
        for (side, expected) in [
            (Side::Ours, "one\nMAIN\nthree\n"),
            (Side::Theirs, "one\nFEATURE\nthree\n"),
        ] {
            let (dir, repo) = clashing_repo();
            assert!(repo.merge("feature").is_err());
            repo.resolve_with_side("f.txt", side).unwrap();
            repo.continue_operation().unwrap();
            assert_eq!(
                fs::read_to_string(dir.path().join("f.txt")).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn continuing_with_conflicts_left_is_refused_and_changes_nothing() {
        let (_dir, repo) = clashing_repo();
        assert!(repo.merge("feature").is_err());
        assert!(matches!(
            repo.continue_operation(),
            Err(Error::Conflicts(_))
        ));
        assert!(repo.in_progress().unwrap().is_some());
    }

    #[test]
    fn resolving_a_file_that_is_not_in_conflict_is_refused() {
        let (dir, repo) = clashing_repo();
        commit_file(dir.path(), "other.txt", "x\n");
        assert!(repo.merge("feature").is_err());
        fs::write(dir.path().join("other.txt"), "changed\n").unwrap();
        assert!(
            repo.resolve_with_text("other.txt", "overwritten\n")
                .is_err()
        );
        assert!(repo.resolve_with_side("other.txt", Side::Theirs).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("other.txt")).unwrap(),
            "changed\n"
        );
    }

    #[test]
    fn a_folder_swapped_for_a_symlink_cannot_redirect_a_resolution_outside_the_repo() {
        let dir = new_repo();
        fs::create_dir(dir.path().join("sub")).unwrap();
        commit_file(dir.path(), "sub/f.txt", "one\n");
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("sub/f.txt"), "feature\n").unwrap();
        git(dir.path(), &["commit", "-qam", "feature edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("sub/f.txt"), "main\n").unwrap();
        git(dir.path(), &["commit", "-qam", "main edit"]);
        let repo = Repo::open(dir.path()).unwrap();
        assert!(matches!(repo.merge("feature"), Err(Error::Conflicts(_))));

        // After git listed the conflict, the folder becomes a link to somewhere else.
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("f.txt"), "outside\n").unwrap();
        fs::remove_dir_all(dir.path().join("sub")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("sub")).unwrap();

        assert!(matches!(
            repo.resolve_with_text("sub/f.txt", "pwned\n"),
            Err(Error::UnsafePath(_))
        ));
        assert!(matches!(
            repo.conflict_text("sub/f.txt"),
            Err(Error::UnsafePath(_))
        ));
        assert!(matches!(
            repo.resolve_with_side("sub/f.txt", Side::Ours),
            Err(Error::UnsafePath(_))
        ));
        assert_eq!(
            fs::read_to_string(outside.path().join("f.txt")).unwrap(),
            "outside\n"
        );
    }

    #[test]
    fn a_conflicted_file_swapped_for_a_symlink_is_neither_read_nor_written_through() {
        let (dir, repo) = clashing_repo();
        assert!(repo.merge("feature").is_err());
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "TOP SECRET\n").unwrap();
        fs::remove_file(dir.path().join("f.txt")).unwrap();
        std::os::unix::fs::symlink(&secret, dir.path().join("f.txt")).unwrap();

        // Reading must not hand back the target's contents.
        assert_eq!(repo.conflict_text("f.txt").unwrap(), None);
        // Writing must not follow the link.
        assert!(repo.resolve_with_text("f.txt", "pwned\n").is_err());
        assert_eq!(fs::read_to_string(&secret).unwrap(), "TOP SECRET\n");
    }

    /// `feature` deletes `gone.txt` while `main` edits it.
    fn delete_modify_repo() -> (TempDir, Repo) {
        let dir = new_repo();
        commit_file(dir.path(), "gone.txt", "keep me\n");
        commit_file(dir.path(), "stay.txt", "s\n");
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        git(dir.path(), &["rm", "-q", "gone.txt"]);
        git(dir.path(), &["commit", "-qm", "delete it"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("gone.txt"), "edited here\n").unwrap();
        git(dir.path(), &["commit", "-qam", "edit it"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn a_file_deleted_by_them_can_be_kept_or_deleted() {
        let (dir, repo) = delete_modify_repo();
        assert!(repo.merge("feature").is_err());
        assert_eq!(
            repo.conflict_kind("gone.txt").unwrap(),
            ConflictKind::DeletedByThem
        );
        repo.resolve_keep("gone.txt").unwrap();
        repo.continue_operation().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("gone.txt")).unwrap(),
            "edited here\n"
        );

        let (dir, repo) = delete_modify_repo();
        assert!(repo.merge("feature").is_err());
        repo.resolve_delete("gone.txt").unwrap();
        repo.continue_operation().unwrap();
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn a_cherry_pick_continues_into_a_normal_commit() {
        let (dir, repo) = clashing_repo();
        let feature_tip = {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "feature"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let commit = repo
            .log(0, 50)
            .unwrap()
            .into_iter()
            .chain({
                git(dir.path(), &["switch", "-q", "feature"]);
                let all = repo.log(0, 50).unwrap();
                git(dir.path(), &["switch", "-q", "main"]);
                all
            })
            .find(|c| c.id == feature_tip)
            .unwrap();

        assert!(matches!(
            repo.cherry_pick(&commit),
            Err(Error::Conflicts(_))
        ));
        repo.resolve_with_side("f.txt", Side::Theirs).unwrap();
        repo.continue_operation().unwrap();

        assert_eq!(repo.in_progress().unwrap(), None);
        assert_eq!(head_parents(dir.path()), 1, "a plain commit, not a merge");
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one\nFEATURE\nthree\n"
        );
    }
}

mod ref_update_hooks {
    //! Writing a ref runs the repository's `reference-transaction` hook. In a repository
    //! that hasn't been trusted, the actions that stay available must not run it.

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A repo whose `reference-transaction` hook drops a marker outside the repo.
    fn hooked_repo() -> (TempDir, TempDir, std::path::PathBuf) {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "one\n");
        let outside = TempDir::new().unwrap();
        let marker = outside.path().join("PWNED");
        let hook = dir.path().join(".git/hooks/reference-transaction");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        (dir, outside, marker)
    }

    fn dirty(dir: &Path) {
        fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    }

    #[test]
    fn stashing_and_dropping_run_no_hooks() {
        let (dir, _outside, marker) = hooked_repo();
        dirty(dir.path());
        let repo = Repo::open(dir.path()).unwrap();

        repo.stash_push("x").unwrap();
        assert!(!marker.exists(), "stash push ran the repository's hook");

        let stash = repo.stashes().unwrap().remove(0);
        repo.stash_drop(&stash).unwrap();
        assert!(!marker.exists(), "stash drop ran the repository's hook");
    }

    #[test]
    fn discarding_and_undoing_run_no_hooks() {
        let (dir, _outside, marker) = hooked_repo();
        dirty(dir.path());
        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let change = status.files.iter().find(|f| f.path == "a.txt").unwrap();

        let backup = repo.discard(&[change]).unwrap();
        assert!(!marker.exists(), "discard ran the repository's hook");

        repo.undo_discard(&backup).unwrap();
        assert!(!marker.exists(), "undo ran the repository's hook");
    }

    #[test]
    fn aborting_a_stopped_operation_runs_no_hooks() {
        let dir = new_repo();
        commit_file(dir.path(), "a.txt", "base\n");
        git(dir.path(), &["switch", "-q", "-c", "other"]);
        fs::write(dir.path().join("a.txt"), "other\n").unwrap();
        git(dir.path(), &["commit", "-qam", "other"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "main\n").unwrap();
        git(dir.path(), &["commit", "-qam", "main"]);
        let repo = Repo::open(dir.path()).unwrap();
        assert!(repo.merge("other").is_err());

        // The hook is planted after the merge stopped, as a hostile folder would have it.
        let outside = TempDir::new().unwrap();
        let marker = outside.path().join("PWNED");
        let hook = dir.path().join(".git/hooks/reference-transaction");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

        repo.abort_operation().unwrap();
        assert!(!marker.exists(), "abort ran the repository's hook");
        assert!(repo.in_progress().unwrap().is_none());
    }
}

mod hostile_remote_output {
    use git_core::read_lines_for_test;

    #[test]
    fn a_huge_line_is_cut_down_instead_of_filling_memory() {
        let huge = "x".repeat(5_000_000);
        let lines = read_lines_for_test(huge.as_bytes());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].len() <= 8192, "{} bytes kept", lines[0].len());
    }

    #[test]
    fn ordinary_lines_and_carriage_return_progress_still_split() {
        let lines = read_lines_for_test(b"one\ntwo\rthree\n\n");
        assert_eq!(lines, ["one", "two", "three"]);
    }
}

/// A repository at `<tempdir>/repo`, so worktrees made next to it stay inside the tempdir.
fn new_repo_in_parent() -> (TempDir, PathBuf) {
    isolate_from_host_git_config();
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    commit_file(&root, "a.txt", "a");
    (parent, root)
}

fn add_linked(root: &Path, args: &[&str], path: &Path) {
    let mut all = vec!["worktree", "add", "-q"];
    all.extend_from_slice(args);
    all.push(path.to_str().unwrap());
    git(root, &all);
}

#[test]
fn worktrees_lists_only_the_main_checkout_in_a_plain_repo() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();

    let worktrees = repo.worktrees().unwrap();

    assert_eq!(worktrees.len(), 1);
    assert!(worktrees[0].is_main);
    assert_eq!(
        worktrees[0].path.canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
    assert_eq!(worktrees[0].head, Head::Branch("main".into()));
}

#[test]
fn worktrees_lists_linked_ones_with_branch_detached_locked_and_missing() {
    let (parent, root) = new_repo_in_parent();
    let p = parent.path();
    add_linked(&root, &["-b", "feat/x"], &p.join("feat"));
    add_linked(&root, &["--detach"], &p.join("det"));
    add_linked(&root, &["-b", "l"], &p.join("locked"));
    git(&root, &["worktree", "lock", p.join("locked").to_str().unwrap()]);
    add_linked(&root, &["-b", "gone"], &p.join("gone"));
    fs::remove_dir_all(p.join("gone")).unwrap();

    let worktrees = Repo::open(&root).unwrap().worktrees().unwrap();

    assert!(worktrees[0].is_main);
    let by_name = |n: &str| worktrees.iter().find(|w| w.path.ends_with(n)).unwrap();
    assert_eq!(by_name("feat").head, Head::Branch("feat/x".into()));
    assert!(matches!(&by_name("det").head, Head::Detached(sha) if sha.len() == 7));
    assert!(by_name("locked").locked);
    assert!(by_name("gone").missing);
    let feat = by_name("feat");
    assert!(!feat.is_main && !feat.locked && !feat.missing);
}

#[test]
fn worktrees_lists_a_folder_whose_name_has_spaces() {
    let (parent, root) = new_repo_in_parent();
    add_linked(&root, &["-b", "s"], &parent.path().join("my worktree ü"));

    let worktrees = Repo::open(&root).unwrap().worktrees().unwrap();

    assert!(worktrees.iter().any(|w| w.path.ends_with("my worktree ü")));
}

#[test]
fn worktrees_from_a_linked_worktree_still_put_the_main_one_first() {
    let (parent, root) = new_repo_in_parent();
    let linked = parent.path().join("linked");
    add_linked(&root, &["-b", "b"], &linked);

    let worktrees = Repo::open(&linked).unwrap().worktrees().unwrap();

    assert!(worktrees[0].is_main);
    assert_eq!(
        worktrees[0].path.canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
}

fn clone_into(parent: &Path, root: &Path) -> PathBuf {
    let clone = parent.join("clone");
    git(
        parent,
        &["clone", "-q", root.to_str().unwrap(), clone.to_str().unwrap()],
    );
    clone
}

#[test]
fn default_base_follows_origin_head() {
    let (parent, root) = new_repo_in_parent();
    git(&root, &["branch", "develop"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/develop"]);
    let clone = clone_into(parent.path(), &root);

    let base = Repo::open(&clone).unwrap().default_base().unwrap();

    assert_eq!(base, "origin/develop");
}

#[test]
fn default_base_falls_back_to_origin_main_without_origin_head() {
    let (parent, root) = new_repo_in_parent();
    let clone = clone_into(parent.path(), &root);
    git(&clone, &["remote", "set-head", "origin", "-d"]);

    let base = Repo::open(&clone).unwrap().default_base().unwrap();

    assert_eq!(base, "origin/main");
}

#[test]
fn default_base_is_local_main_without_a_remote() {
    let (_parent, root) = new_repo_in_parent();

    let base = Repo::open(&root).unwrap().default_base().unwrap();

    assert_eq!(base, "main");
}

#[test]
fn default_base_is_local_master_when_there_is_no_main() {
    let (_parent, root) = new_repo_in_parent();
    git(&root, &["branch", "-m", "main", "master"]);

    let base = Repo::open(&root).unwrap().default_base().unwrap();

    assert_eq!(base, "master");
}

#[test]
fn default_base_errors_when_no_main_branch_exists() {
    let (_parent, root) = new_repo_in_parent();
    git(&root, &["branch", "-m", "main", "trunk"]);

    let result = Repo::open(&root).unwrap().default_base();

    assert!(matches!(result, Err(Error::NoDefaultBranch)));
}

#[test]
fn common_dir_is_the_same_from_main_and_linked_worktrees() {
    let (parent, root) = new_repo_in_parent();
    let linked = parent.path().join("linked");
    add_linked(&root, &["-b", "b"], &linked);

    let main_dir = Repo::open(&root).unwrap().common_dir().unwrap();
    let linked_dir = Repo::open(&linked).unwrap().common_dir().unwrap();

    assert_eq!(main_dir, linked_dir);
    assert_eq!(main_dir, root.join(".git").canonicalize().unwrap());
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    isolate_from_host_git_config();
    let output = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn has_local_branch(repo: &Repo, name: &str) -> bool {
    repo.branches().unwrap().local.iter().any(|b| b.name == name)
}

#[test]
fn add_worktree_creates_the_branch_off_main_next_to_the_repo() {
    let (parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();

    let path = repo.add_worktree("feat/one", "feat-one").unwrap();

    assert_eq!(
        path.canonicalize().unwrap(),
        parent.path().join("feat-one").canonicalize().unwrap()
    );
    let status = Repo::open(&path).unwrap().status().unwrap();
    assert_eq!(status.branch.head, Head::Branch("feat/one".into()));
    assert_eq!(status.branch.upstream, None);
}

#[test]
fn add_worktree_starts_from_origin_main_not_the_current_branch() {
    let (parent, root) = new_repo_in_parent();
    let clone = clone_into(parent.path(), &root);
    git(&clone, &["switch", "-q", "-c", "work"]);
    commit_file(&clone, "w.txt", "w");
    let repo = Repo::open(&clone).unwrap();

    let path = repo.add_worktree("b", "b").unwrap();

    assert_eq!(
        git_out(&path, &["rev-parse", "HEAD"]),
        git_out(&clone, &["rev-parse", "origin/main"])
    );
}

#[test]
fn add_worktree_from_a_linked_worktree_still_goes_next_to_the_main_one() {
    let (parent, root) = new_repo_in_parent();
    let nested = parent.path().join("inner");
    fs::create_dir(&nested).unwrap();
    let linked = nested.join("linked");
    add_linked(&root, &["-b", "l"], &linked);

    let path = Repo::open(&linked).unwrap().add_worktree("b", "b").unwrap();

    assert_eq!(
        path.parent().unwrap().canonicalize().unwrap(),
        parent.path().canonicalize().unwrap()
    );
}

#[test]
fn add_worktree_turns_slashes_in_the_branch_into_dashes() {
    assert_eq!(worktree_folder_name("feature/neo-1/x"), "feature-neo-1-x");
}

#[test]
fn add_worktree_refuses_an_existing_folder_and_creates_no_branch() {
    let (parent, root) = new_repo_in_parent();
    fs::create_dir(parent.path().join("taken")).unwrap();
    let repo = Repo::open(&root).unwrap();

    let result = repo.add_worktree("b", "taken");

    assert!(matches!(result, Err(Error::FolderExists(_))));
    assert!(!has_local_branch(&repo, "b"));
}

#[test]
fn add_worktree_refuses_an_existing_branch_and_creates_no_folder() {
    let (parent, root) = new_repo_in_parent();
    git(&root, &["branch", "existing"]);
    let repo = Repo::open(&root).unwrap();

    let result = repo.add_worktree("existing", "f");

    assert!(matches!(result, Err(Error::BranchExists(_))));
    assert!(!parent.path().join("f").exists());
}

#[test]
fn add_worktree_rejects_folder_names_that_are_not_one_plain_name() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();

    for bad in ["", ".", "..", "a/b", "-x", "a\0b"] {
        let result = repo.add_worktree("ok", bad);

        assert!(
            matches!(result, Err(Error::InvalidFolderName(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn add_worktree_rejects_an_invalid_branch_name() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();

    for bad in ["-x", "a..b"] {
        let result = repo.add_worktree(bad, "f");

        assert!(
            matches!(result, Err(Error::InvalidBranchName(_))),
            "{bad:?}"
        );
    }
}

fn worktree_at(repo: &Repo, path: &Path) -> Worktree {
    let wanted = path.file_name().unwrap();
    repo.worktrees()
        .unwrap()
        .into_iter()
        .find(|w| w.path.file_name() == Some(wanted))
        .unwrap()
}

#[test]
fn a_clean_new_worktree_has_nothing_to_lose() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check, RemovalCheck::default());
}

#[test]
fn removal_check_counts_changed_staged_and_untracked_files_but_not_ignored_ones() {
    let (_parent, root) = new_repo_in_parent();
    commit_file(&root, ".gitignore", "*.log\n");
    let repo = Repo::open(&root).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();
    fs::write(path.join("a.txt"), "changed").unwrap();
    fs::write(path.join("new.txt"), "new").unwrap();
    git(&path, &["add", "new.txt"]);
    fs::write(path.join("u.txt"), "untracked").unwrap();
    fs::write(path.join("x.log"), "ignored").unwrap();

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.uncommitted_files, 3);
}

#[test]
fn removal_check_counts_a_rename_once() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();
    git(&path, &["mv", "a.txt", "renamed.txt"]);

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.uncommitted_files, 1);
}

#[test]
fn removal_check_counts_commits_only_this_branch_has() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();
    commit_file(&path, "one.txt", "1");
    commit_file(&path, "two.txt", "2");

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.unique_commits, 2);
}

#[test]
fn a_branch_merged_into_main_has_no_unique_commits() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();
    commit_file(&path, "one.txt", "1");
    git(&root, &["merge", "-q", "--ff-only", "b"]);

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.unique_commits, 0);
}

#[test]
fn a_pushed_branch_has_no_unique_commits() {
    let (parent, root) = new_repo_in_parent();
    let clone = clone_into(parent.path(), &root);
    let repo = Repo::open(&clone).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();
    commit_file(&path, "one.txt", "1");
    git(&path, &["push", "-q", "-u", "origin", "b"]);

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.unique_commits, 0);
}

#[test]
fn a_detached_worktree_counts_commits_no_ref_reaches() {
    let (parent, root) = new_repo_in_parent();
    let path = parent.path().join("det");
    add_linked(&root, &["--detach"], &path);
    commit_file(&path, "one.txt", "1");
    let repo = Repo::open(&root).unwrap();

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.unique_commits, 1);
}

#[test]
fn removal_check_is_the_same_from_inside_the_worktree() {
    let (_parent, root) = new_repo_in_parent();
    let main = Repo::open(&root).unwrap();
    let path = main.add_worktree("b", "b").unwrap();
    commit_file(&path, "one.txt", "1");
    let inside = Repo::open(&path).unwrap();

    let from_inside = inside.removal_check(&worktree_at(&inside, &path)).unwrap();
    let from_main = main.removal_check(&worktree_at(&main, &path)).unwrap();

    assert_eq!(from_inside, from_main);
    assert_eq!(from_inside.unique_commits, 1);
}

#[test]
fn a_missing_worktree_reports_no_uncommitted_files() {
    let (_parent, root) = new_repo_in_parent();
    let repo = Repo::open(&root).unwrap();
    let path = repo.add_worktree("b", "b").unwrap();
    commit_file(&path, "one.txt", "1");
    fs::remove_dir_all(&path).unwrap();

    let check = repo.removal_check(&worktree_at(&repo, &path)).unwrap();

    assert_eq!(check.uncommitted_files, 0);
    assert_eq!(check.unique_commits, 1);
}
