use std::{
    path::Path,
    process::{Command, Stdio},
};

use crate::Error;

/// Runs `git` in `dir` and returns stdout. Exit code 0 is success.
///
/// Never prompts for input, and never takes the index lock, so a background
/// refresh cannot make the user's own `git commit` fail.
///
/// A repository's own config is untrusted: opening a cloned repo must not run
/// code it chose. `core.fsmonitor` is forced off because it names a command that
/// `git status` would execute. Diff callers add `--no-ext-diff` and `--no-textconv`
/// for the same reason. Config that only matters for writes (hooks, signing) is
/// left alone so it keeps working for commits.
pub(crate) fn run_git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
    run_git_allowing(dir, args, &[0])
}

/// Like `run_git`, but also treats the listed exit codes as success. `git diff
/// --no-index` exits 1 when the files differ, which is the normal case.
pub(crate) fn run_git_allowing(
    dir: &Path,
    args: &[&str],
    success_codes: &[i32],
) -> Result<Vec<u8>, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .output()?;

    if output
        .status
        .code()
        .is_some_and(|code| success_codes.contains(&code))
    {
        Ok(output.stdout)
    } else {
        Err(Error::Git {
            command: args.first().copied().unwrap_or_default().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}
