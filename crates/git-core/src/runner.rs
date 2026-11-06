use std::{
    path::Path,
    process::{Command, Stdio},
};

use crate::Error;

/// Runs `git` in `dir` and returns stdout.
///
/// Never prompts for input, and never takes the index lock, so a background
/// refresh cannot make the user's own `git commit` fail.
pub(crate) fn run_git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .output()?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::Git {
            command: args.first().copied().unwrap_or_default().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}
