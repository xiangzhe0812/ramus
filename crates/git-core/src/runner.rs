use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use crate::Error;

/// Runs `git` in `dir` and returns stdout. Exit code 0 is success.
///
/// Never prompts for input, and never takes the index lock, so a background
/// refresh cannot make the user's own `git commit` fail.
///
/// A repository's own config is untrusted: opening a directory that came with its
/// `.git` must not run code it chose. Two defences apply to every call:
/// - `core.fsmonitor` is forced off, because it names a command `git status` runs.
/// - Filter drivers (`filter.<name>.clean/smudge/process`) defined in the repo's own
///   config are blanked, because git runs them whenever it compares or stages a
///   file. Filters from the user's global or system config are left alone, as are
///   `git-lfs` ones, so legitimate setups keep working.
///
/// Diff callers also pass `--no-ext-diff` and `--no-textconv`. Hooks are left alone so
/// they still run for commits, as they would in a terminal.
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
    run(dir, args, None, success_codes)
}

/// Runs `git` with `input` on stdin, e.g. `git commit -F -` so a message never
/// touches the command line or a temp file.
pub(crate) fn run_git_with_input(
    dir: &Path,
    args: &[&str],
    input: &[u8],
) -> Result<Vec<u8>, Error> {
    run(dir, args, Some(input), &[0])
}

fn run(
    dir: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    success_codes: &[i32],
) -> Result<Vec<u8>, Error> {
    let overrides = untrusted_filter_overrides(dir);
    exec(dir, &overrides, args, input, success_codes)
}

/// `-c filter.<name>.<command>=` arguments that disable filters the repo configured.
fn untrusted_filter_overrides(dir: &Path) -> Vec<String> {
    // Reading config runs nothing, so this call itself needs no overrides.
    let Ok(output) = exec(
        dir,
        &[],
        &["config", "--show-scope", "--get-regexp", r"^filter\."],
        None,
        &[0, 1],
    ) else {
        return Vec::new();
    };
    blank_untrusted_filters(&String::from_utf8_lossy(&output))
}

/// Parses `git config --show-scope --get-regexp` output (`scope<TAB>key value`).
fn blank_untrusted_filters(config: &str) -> Vec<String> {
    let mut overrides = Vec::new();
    for line in config.lines() {
        let Some((scope, entry)) = line.split_once('\t') else {
            continue;
        };
        if !matches!(scope, "local" | "worktree") {
            continue;
        }
        let (key, value) = entry.split_once(' ').unwrap_or((entry, ""));
        let Some((driver, command)) = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
        else {
            continue;
        };
        if !matches!(command, "clean" | "smudge" | "process") || is_lfs_command(value) {
            continue;
        }
        overrides.push("-c".to_owned());
        overrides.push(format!("filter.{driver}.{command}="));
    }
    overrides
}

/// Git LFS is the one common filter installed per repository, and blanking it
/// would make every large file look modified.
fn is_lfs_command(command: &str) -> bool {
    command.trim_start().starts_with("git-lfs ")
}

fn exec(
    dir: &Path,
    overrides: &[String],
    args: &[&str],
    input: Option<&[u8]>,
    success_codes: &[i32],
) -> Result<Vec<u8>, Error> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(overrides)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // A file named `a[1].txt` must mean that file, not a glob.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // Dropping `stdin` at the end of the block closes the pipe so git sees EOF.
        stdin.write_all(input)?;
    }
    let output = child.wait_with_output()?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanks_repo_scoped_filters_only() {
        let config = "local\tfilter.evil.clean /tmp/evil.sh\n\
                      global\tfilter.mine.clean my-clean\n\
                      worktree\tfilter.wt.smudge /tmp/wt.sh\n\
                      system\tfilter.sys.process sys-filter\n";
        assert_eq!(
            blank_untrusted_filters(config),
            ["-c", "filter.evil.clean=", "-c", "filter.wt.smudge="]
        );
    }

    #[test]
    fn keeps_git_lfs_filters() {
        let config = "local\tfilter.lfs.clean git-lfs clean -- %f\n\
                      local\tfilter.lfs.process git-lfs filter-process\n\
                      local\tfilter.lfs.required true\n";
        assert!(blank_untrusted_filters(config).is_empty());
    }

    #[test]
    fn only_clean_smudge_and_process_are_blanked() {
        let config = "local\tfilter.x.required true\nlocal\tfilter.x.clean run-me\n";
        assert_eq!(blank_untrusted_filters(config), ["-c", "filter.x.clean="]);
    }

    #[test]
    fn driver_names_containing_dots_are_handled() {
        let config = "local\tfilter.my.driver.clean run-me\n";
        assert_eq!(
            blank_untrusted_filters(config),
            ["-c", "filter.my.driver.clean="]
        );
    }

    #[test]
    fn malformed_lines_are_ignored() {
        assert!(blank_untrusted_filters("garbage\n\nlocal\tnot-a-filter x\n").is_empty());
    }
}
