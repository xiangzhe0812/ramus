//! Fetch, pull and push: operations that talk to a remote, so they can be slow, can fail
//! for reasons outside the app, and must be cancellable.

use std::{
    io::{BufReader, Read},
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    Error, OperationKind, Repo,
    runner::{defended_command, run_git},
};

/// Shared flag a caller sets to stop a running operation.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How far along a transfer is, as git reports it ("Receiving objects", 62%).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub phase: String,
    pub percent: u8,
}

/// Parses a git progress line such as `Receiving objects:  62% (31/50), 1.2 MiB`
/// or `remote: Compressing objects: 100% (4/4), done.`.
pub(crate) fn parse_progress(line: &str) -> Option<Progress> {
    let line = line
        .trim()
        .strip_prefix("remote:")
        .unwrap_or(line.trim())
        .trim();
    let (phase, rest) = line.split_once(':')?;
    let percent: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    // Must be digits then `%`, so "To host:22 something" is not mistaken for progress.
    if percent.is_empty() || !rest.trim_start()[percent.len()..].starts_with('%') {
        return None;
    }
    if phase.is_empty() || !phase.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') {
        return None;
    }
    Some(Progress {
        phase: phase.trim().to_owned(),
        percent: percent.parse::<u8>().ok()?.min(100),
    })
}

/// Splits a byte stream into lines on either `\r` or `\n`: git redraws a progress line
/// in place with `\r`.
fn read_lines(reader: impl Read, send: mpsc::Sender<String>) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while let Ok(1) = reader.read(&mut byte) {
        if byte[0] == b'\r' || byte[0] == b'\n' {
            if !line.is_empty() {
                let _ = send.send(String::from_utf8_lossy(&line).into_owned());
                line.clear();
            }
        } else {
            line.push(byte[0]);
        }
    }
    if !line.is_empty() {
        let _ = send.send(String::from_utf8_lossy(&line).into_owned());
    }
}

/// Stops git and everything it started (ssh, hooks): they share a process group because
/// git was started in its own session.
fn terminate(child: &mut std::process::Child) {
    let group = child.id() as i32;
    // SAFETY: `killpg` only sends a signal to the process group we created.
    unsafe { libc::killpg(group, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: as above.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let _ = child.wait();
}

/// Runs a `git` command that talks to a remote, reporting progress and honouring `cancel`.
///
/// Git is started in its own session, so it has no terminal: ssh and credential prompts
/// can't block waiting for someone to type, and fail instead.
fn run_network(
    repo: &Repo,
    args: &[&str],
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), Error> {
    let command = defended_command(repo.root(), args)?;
    run_command(
        command,
        args.first().copied().unwrap_or_default(),
        cancel,
        on_progress,
    )
}

/// Runs a prepared network `command` (named `name` in errors): progress, cancellation and
/// the no-terminal session described on [`run_network`].
fn run_command(
    mut command: std::process::Command,
    name: &str,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), Error> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // The `ext::` transport runs a command, and a remote's address can be set by a
        // repository's own config, so every network command is limited to real protocols.
        .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh");
    // SAFETY: `setsid` is async-signal-safe, which is all `pre_exec` allows.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;

    let stderr = child.stderr.take().expect("stderr was piped");
    let (send, lines) = mpsc::channel();
    thread::spawn(move || read_lines(stderr, send));

    let mut messages: Vec<String> = Vec::new();
    let mut handle = |line: String, messages: &mut Vec<String>| match parse_progress(&line) {
        Some(progress) => on_progress(progress),
        None => messages.push(line),
    };

    let status = loop {
        while let Ok(line) = lines.try_recv() {
            handle(line, &mut messages);
        }
        if cancel.is_cancelled() {
            terminate(&mut child);
            return Err(Error::Cancelled);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Whatever git wrote just before exiting. A grandchild holding the pipe open must not
    // make us wait forever, hence the timeout.
    while let Ok(line) = lines.recv_timeout(Duration::from_millis(200)) {
        handle(line, &mut messages);
    }

    if status.success() {
        Ok(())
    } else {
        Err(Error::Git {
            command: name.to_owned(),
            stderr: messages.join("\n").trim().to_owned(),
        })
    }
}

impl Repo {
    /// The names of the configured remotes.
    pub fn remotes(&self) -> Result<Vec<String>, Error> {
        let output = run_git(self.root(), &["remote"])?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .filter(|name| !name.is_empty())
            .collect())
    }

    /// Gives a repository with no remote one called `origin`. Refuses if it has any, so it
    /// can't silently redirect where an existing remote points.
    pub fn add_origin(&self, url: &str) -> Result<(), Error> {
        let url = url.trim();
        if url.is_empty() || url.starts_with('-') {
            return Err(Error::InvalidUrl(url.to_owned()));
        }
        if !self.remotes()?.is_empty() {
            return Err(Error::Git {
                command: "remote".into(),
                stderr: "this repository already has a remote".into(),
            });
        }
        run_git(self.root(), &["remote", "add", "--", "origin", url]).map(|_| ())
    }

    /// Downloads new commits and removes remote branches that were deleted upstream.
    pub fn fetch(
        &self,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), Error> {
        run_network(
            self,
            &["fetch", "--prune", "--progress"],
            cancel,
            on_progress,
        )
    }

    /// Fast-forwards the current branch to its upstream. Refuses, changing nothing, if the
    /// branches have diverged: merging or rebasing is a decision for the user.
    pub fn pull(
        &self,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), Error> {
        run_network(
            self,
            &["pull", "--ff-only", "--progress"],
            cancel,
            on_progress,
        )
    }

    /// Brings in the upstream's commits even when the branches have diverged, joining them
    /// with a merge commit. A conflict leaves the merge in progress, to resolve or abort.
    pub fn pull_merge(
        &self,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), Error> {
        let result = run_network(
            self,
            &["pull", "--no-rebase", "--no-edit", "--progress"],
            cancel,
            on_progress,
        );
        // A conflict leaves the merge in progress, to be resolved or aborted.
        result.map_err(|error| match error {
            Error::Git { .. } => self.stopped_or(OperationKind::Merge, error),
            other => other,
        })
    }

    /// Pushes the current branch to its upstream.
    pub fn push(
        &self,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), Error> {
        run_network(self, &["push", "--progress"], cancel, on_progress)
    }

    /// Pushes the current branch for the first time and makes the remote branch its
    /// upstream. Uses `origin`, or the only remote there is.
    pub fn publish(
        &self,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), Error> {
        let remotes = self.remotes()?;
        let remote = match remotes.as_slice() {
            [] => return Err(Error::NoRemote),
            [only] => only.as_str(),
            many if many.iter().any(|name| name == "origin") => "origin",
            many => return Err(Error::AmbiguousRemote(many.to_vec())),
        };
        if remote.starts_with('-') {
            return Err(Error::InvalidBranchName(remote.to_owned()));
        }
        run_network(
            self,
            &["push", "--progress", "--set-upstream", remote, "HEAD"],
            cancel,
            on_progress,
        )
    }
}

impl Repo {
    /// Clones `url` into `destination` and opens the result.
    ///
    /// `destination` must not exist, or be an empty folder; anything else is refused
    /// untouched. A clone that fails or is cancelled removes the folder this call made.
    ///
    /// The URL comes from the user, but may have come from a web page, so git is kept from
    /// treating it as an option (`--`) and from the `ext::` transport, which runs a
    /// command. Nothing the remote sends can set config, so a fresh clone is as trusted as
    /// the user's own settings.
    pub fn clone_from(
        url: &str,
        destination: &Path,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Repo, Error> {
        let url = url.trim();
        if url.is_empty() {
            return Err(Error::InvalidUrl(url.to_owned()));
        }
        let existed = destination.exists();
        if existed {
            let empty = destination
                .read_dir()
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                return Err(Error::DestinationNotEmpty(destination.to_owned()));
            }
        }
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;

        let mut command = std::process::Command::new("git");
        command
            .current_dir(parent)
            .args(["-c", "core.fsmonitor=false"])
            .args(["-c", "protocol.ext.allow=never"])
            .args(["clone", "--progress", "--", url])
            .arg(destination)
            .env("GIT_TERMINAL_PROMPT", "0");
        let result = run_command(command, "clone", cancel, on_progress);
        if result.is_err() && !existed {
            // Git cleans up after itself on an error, but not when it is killed.
            let _ = std::fs::remove_dir_all(destination);
        }
        result?;
        Repo::open(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(phase: &str, percent: u8) -> Option<Progress> {
        Some(Progress {
            phase: phase.into(),
            percent,
        })
    }

    #[test]
    fn parses_progress_lines_with_and_without_the_remote_prefix() {
        assert_eq!(
            parse_progress("Receiving objects:  62% (31/50), 1.2 MiB | 2.0 MiB/s"),
            progress("Receiving objects", 62)
        );
        assert_eq!(
            parse_progress("remote: Compressing objects: 100% (4/4), done."),
            progress("Compressing objects", 100)
        );
        assert_eq!(
            parse_progress("Resolving deltas:   0% (0/9)"),
            progress("Resolving deltas", 0)
        );
    }

    #[test]
    fn ordinary_messages_are_not_progress() {
        for line in [
            "To https://example.com/repo.git",
            " ! [rejected]        main -> main (fetch first)",
            "error: failed to push some refs to 'host:22/repo.git'",
            "fatal: Authentication failed for 'https://x'",
            "remote: Resolving: not a percentage",
            "Receiving objects: soon",
            "",
        ] {
            assert_eq!(parse_progress(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_percentage_above_one_hundred_is_clamped() {
        assert_eq!(
            parse_progress("Writing objects: 250% (1/1)"),
            progress("Writing objects", 100)
        );
    }

    #[test]
    fn carriage_returns_split_lines_like_newlines() {
        let (send, receive) = mpsc::channel();
        read_lines(
            &b"Receiving objects:  10%\rReceiving objects:  50%\rdone\nlast"[..],
            send,
        );
        let lines: Vec<_> = receive.try_iter().collect();
        assert_eq!(
            lines,
            [
                "Receiving objects:  10%",
                "Receiving objects:  50%",
                "done",
                "last"
            ]
        );
    }

    #[test]
    fn a_cancel_token_is_shared_between_clones() {
        let token = CancelToken::new();
        let other = token.clone();
        assert!(!other.is_cancelled());
        token.cancel();
        assert!(other.is_cancelled());
    }
}
