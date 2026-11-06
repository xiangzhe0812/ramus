use std::{fs, path::Path, process::Command, sync::Once};

use git_core::{ChangeKind, Error, Head, Repo};
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
        // Different size, so git has to look at the content rather than trust the stat.
        fs::write(dir.path().join("a.txt"), "one and a bit more\n").unwrap();
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
