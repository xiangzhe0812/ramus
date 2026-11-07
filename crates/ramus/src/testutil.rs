//! Helpers shared by the tests: run real `git` in a temporary directory.

use std::{path::Path, process::Command};

/// Runs `git` in `dir` with a fixed identity, returning whether it succeeded.
pub fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@e.com"])
        .args(args)
        .output()
        .expect("git can run")
        .status
        .success()
}

/// Like [`git_ok`], but a failure fails the test.
pub fn git(dir: &Path, args: &[&str]) {
    assert!(git_ok(dir, args), "git {args:?} failed");
}
