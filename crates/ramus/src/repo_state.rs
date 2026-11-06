use std::path::PathBuf;

use git_core::{DiffHunk, Repo, RepoStatus};
use gpui::{Context, EventEmitter, SharedString, Task};

/// Things views react to that aren't just "state changed".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoEvent {
    /// A commit succeeded, so the commit message can be cleared.
    Committed,
}

/// The open repository and its latest status, shared by every view.
///
/// Git runs on a background executor; views only ever read the last result.
#[derive(Default)]
pub struct RepoState {
    repo: Option<Repo>,
    status: Option<RepoStatus>,
    error: Option<SharedString>,
    /// Bumped on every load so a slow, older load can't overwrite a newer one.
    generation: u64,
    load: Option<Task<()>>,
    /// A write (stage, unstage, commit) is running. Writes are never cancelled
    /// by a refresh, and only one runs at a time.
    action: Option<Task<()>>,
}

impl EventEmitter<RepoEvent> for RepoState {}

impl RepoState {
    pub fn repo(&self) -> Option<&Repo> {
        self.repo.as_ref()
    }

    pub fn status(&self) -> Option<&RepoStatus> {
        self.status.as_ref()
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn is_busy(&self) -> bool {
        self.action.is_some()
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// Opens the repository containing `path`. On failure the current repo stays open.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load(None, path, true, cx);
    }

    /// Re-reads status for the open repository, if any.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.clone() {
            let root = repo.root().to_owned();
            self.load(Some(repo), root, false, cx);
        }
    }

    /// `clear_error` is false for background refreshes: they must not hide the error
    /// from a write that just failed.
    fn load(
        &mut self,
        known: Option<Repo>,
        path: PathBuf,
        clear_error: bool,
        cx: &mut Context<Self>,
    ) {
        self.generation += 1;
        let generation = self.generation;

        self.load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let repo = match known {
                        Some(repo) => repo,
                        None => Repo::open(&path)?,
                    };
                    let status = repo.status()?;
                    Ok::<_, git_core::Error>((repo, status))
                })
                .await;

            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok((repo, status)) => {
                        this.repo = Some(repo);
                        this.status = Some(status);
                        if clear_error {
                            this.error = None;
                        }
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn stage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| repo.stage(&as_strs(&paths)));
    }

    pub fn unstage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| repo.unstage(&as_strs(&paths)));
    }

    /// Stages just `hunks` of `path`'s unstaged changes.
    pub fn stage_hunks(&mut self, path: String, hunks: Vec<DiffHunk>, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| {
            repo.stage_hunks(&path, &hunks.iter().collect::<Vec<_>>())
        });
    }

    /// Unstages just `hunks` of `path`'s staged changes.
    pub fn unstage_hunks(&mut self, path: String, hunks: Vec<DiffHunk>, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| {
            repo.unstage_hunks(&path, &hunks.iter().collect::<Vec<_>>())
        });
    }

    pub fn stage_all(&mut self, cx: &mut Context<Self>) {
        self.write(cx, None, |repo| repo.stage_all());
    }

    pub fn unstage_all(&mut self, cx: &mut Context<Self>) {
        self.write(cx, None, |repo| repo.unstage_all());
    }

    pub fn commit(&mut self, summary: String, description: String, cx: &mut Context<Self>) {
        self.write(cx, Some(RepoEvent::Committed), move |repo| {
            repo.commit(&summary, &description)
        });
    }

    /// Runs a write on a background thread, then reloads status. Ignored while another
    /// write is running, so a double click can't stage or commit twice.
    fn write(
        &mut self,
        cx: &mut Context<Self>,
        on_success: Option<RepoEvent>,
        work: impl FnOnce(&Repo) -> Result<(), git_core::Error> + Send + 'static,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.action.is_some() {
            return;
        }

        self.action = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { work(&repo) })
                .await;

            this.update(cx, |this, cx| {
                this.action = None;
                match result {
                    Ok(()) => {
                        this.error = None;
                        if let Some(event) = on_success {
                            cx.emit(event);
                        }
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                // Reload even after a failure: a hook may have changed things.
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

fn as_strs(paths: &[String]) -> Vec<&str> {
    paths.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

    use gpui::{AppContext, TestAppContext};
    use tempfile::TempDir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@e.com"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repo with one commit and a hook that rejects every commit.
    fn repo_with_rejecting_hook() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);

        let hook = dir.path().join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\necho 'lint failed' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        dir
    }

    #[gpui::test]
    async fn a_failed_commit_keeps_its_error_through_the_reload_that_follows(
        cx: &mut TestAppContext,
    ) {
        let dir = repo_with_rejecting_hook();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.stage(vec!["b.txt".into()], cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| {
            state.commit("blocked".into(), String::new(), cx)
        });
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            let error = state
                .error()
                .expect("the failed commit should leave an error");
            assert!(error.contains("lint failed"), "{error}");
            assert!(!state.is_busy());
            // The staged file is still staged, so nothing was lost.
            assert!(
                state
                    .status()
                    .unwrap()
                    .files
                    .iter()
                    .any(|f| f.is_fully_staged())
            );
        });
    }

    #[gpui::test]
    async fn the_next_successful_action_clears_the_error(cx: &mut TestAppContext) {
        let dir = repo_with_rejecting_hook();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.stage(vec!["b.txt".into()], cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| {
            state.commit("blocked".into(), String::new(), cx)
        });
        cx.executor().run_until_parked();
        assert!(state.read_with(cx, |state, _| state.error().is_some()));

        state.update(cx, |state, cx| state.unstage(vec!["b.txt".into()], cx));
        cx.executor().run_until_parked();
        assert!(state.read_with(cx, |state, _| state.error().is_none()));
    }

    #[gpui::test]
    async fn opening_a_folder_that_is_not_a_repo_reports_it_and_keeps_the_open_repo(
        cx: &mut TestAppContext,
    ) {
        let repo = repo_with_rejecting_hook();
        let not_a_repo = TempDir::new().unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(repo.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.open(not_a_repo.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            assert!(state.error().is_some());
            assert_eq!(
                state.repo().unwrap().name(),
                repo.path().file_name().unwrap().to_string_lossy()
            );
        });
    }

    #[gpui::test]
    async fn a_second_write_while_one_is_running_is_ignored(cx: &mut TestAppContext) {
        let dir = repo_with_rejecting_hook();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| {
            state.stage(vec!["b.txt".into()], cx);
            assert!(state.is_busy());
            // Would unstage it again if it were allowed to run.
            state.unstage(vec!["b.txt".into()], cx);
        });
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            assert!(
                state
                    .status()
                    .unwrap()
                    .files
                    .iter()
                    .any(|f| f.is_fully_staged())
            );
        });
    }
}
