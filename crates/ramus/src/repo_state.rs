use std::path::PathBuf;

use crate::settings::{self, Settings};

use git_core::{Branch, BranchList, DiffHunk, Repo, RepoStatus};
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
    settings: Settings,
    /// Where `settings` is saved, or `None` to keep them in memory only (tests).
    settings_path: Option<PathBuf>,
    /// The open repository may run programs its own config names (hooks, credential
    /// helpers). Until it is trusted, commit and network operations are refused.
    trusted: bool,
    /// Branches for the branch switcher, loaded when it opens.
    branches: Option<BranchList>,
    branches_task: Option<Task<()>>,
}

impl EventEmitter<RepoEvent> for RepoState {}

impl Default for RepoState {
    /// Settings are kept in memory only. Use [`RepoState::persistent`] in the app.
    fn default() -> Self {
        Self::with_settings(Settings::default(), None)
    }
}

impl RepoState {
    /// State whose settings are loaded from, and saved to, the user's settings file.
    pub fn persistent() -> Self {
        match settings::default_path() {
            Some(path) => Self::with_settings(Settings::load(&path), Some(path)),
            None => Self::default(),
        }
    }

    pub fn with_settings(settings: Settings, settings_path: Option<PathBuf>) -> Self {
        Self {
            repo: None,
            status: None,
            error: None,
            generation: 0,
            load: None,
            action: None,
            settings,
            settings_path,
            trusted: false,
            branches: None,
            branches_task: None,
        }
    }

    pub fn is_trusted(&self) -> bool {
        self.trusted
    }

    /// Lets the open repository run its own configured programs, and remembers that.
    pub fn trust(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = &self.repo else {
            return;
        };
        self.settings.trust(repo.root());
        self.trusted = true;
        if let Some(path) = &self.settings_path
            && let Err(error) = self.settings.save(path)
        {
            // Trusted for this session; the choice just won't be remembered.
            self.error = Some(format!("Couldn't save the trust setting: {error}").into());
        }
        cx.notify();
    }

    pub fn branches(&self) -> Option<&BranchList> {
        self.branches.as_ref()
    }

    /// Reads the branch list in the background; the switcher shows it when it arrives.
    pub fn load_branches(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.branches_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.branches() })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(branches) => this.branches = Some(branches),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Whether `action` may run: refused, with an explanation, in a repository that
    /// hasn't been trusted, because checkouts and commits run the repo's own hooks.
    fn require_trust(&mut self, action: &str, cx: &mut Context<Self>) -> bool {
        if !self.trusted {
            self.error =
                Some(format!("Trust this repository to {action}: its hooks would run.").into());
            cx.notify();
        }
        self.trusted
    }

    pub fn switch_branch(&mut self, branch: Branch, cx: &mut Context<Self>) {
        if self.require_trust("switch branches", cx) {
            self.branches = None;
            self.write(cx, None, move |repo| repo.switch_branch(&branch));
        }
    }

    pub fn create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        if self.require_trust("create a branch", cx) {
            self.branches = None;
            self.write(cx, None, move |repo| repo.create_branch(&name));
        }
    }

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
                        this.trusted = this.settings.is_trusted(repo.root());
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
        // Committing runs the repository's hooks, which are programs it chose.
        if !self.require_trust("commit", cx) {
            return;
        }
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
        state.update(cx, |state, cx| state.trust(cx));

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
        state.update(cx, |state, cx| state.trust(cx));
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

    fn commit_count(dir: &Path) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// A repo with an untracked file ready to stage and no hook, so a refusal to commit
    /// can only come from trust.
    fn repo_without_hooks() -> TempDir {
        let dir = repo_with_rejecting_hook();
        fs::remove_file(dir.path().join(".git/hooks/pre-commit")).unwrap();
        dir
    }

    #[gpui::test]
    async fn a_repository_is_untrusted_until_the_user_trusts_it(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();

        assert!(!state.read_with(cx, |state, _| state.is_trusted()));
    }

    #[gpui::test]
    async fn an_untrusted_repository_refuses_to_commit_and_says_why(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.stage(vec!["b.txt".into()], cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| {
            state.commit("nope".into(), String::new(), cx)
        });
        cx.executor().run_until_parked();

        assert_eq!(
            commit_count(dir.path()),
            1,
            "no commit should have been made"
        );
        state.read_with(cx, |state, _| {
            assert!(state.error().unwrap().contains("Trust this repository"));
        });
    }

    #[gpui::test]
    async fn after_trusting_the_repository_commits_go_through(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        state.update(cx, |state, cx| state.stage(vec!["b.txt".into()], cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| {
            state.commit("yes".into(), String::new(), cx)
        });
        cx.executor().run_until_parked();

        assert_eq!(commit_count(dir.path()), 2);
    }

    #[gpui::test]
    async fn trust_is_remembered_across_restarts_but_only_for_that_repository(
        cx: &mut TestAppContext,
    ) {
        let settings_dir = TempDir::new().unwrap();
        let settings_path = settings_dir.path().join("settings.json");
        let trusted = repo_without_hooks();
        let other = repo_without_hooks();

        let first =
            cx.new(|_| RepoState::with_settings(Settings::default(), Some(settings_path.clone())));
        first.update(cx, |state, cx| state.open(trusted.path().to_owned(), cx));
        cx.executor().run_until_parked();
        first.update(cx, |state, cx| state.trust(cx));
        assert!(
            settings_path.exists(),
            "the choice should be written to disk"
        );

        // A new session reads the same file.
        let second = cx.new(|_| {
            RepoState::with_settings(Settings::load(&settings_path), Some(settings_path.clone()))
        });
        second.update(cx, |state, cx| state.open(trusted.path().to_owned(), cx));
        cx.executor().run_until_parked();
        assert!(second.read_with(cx, |state, _| state.is_trusted()));

        second.update(cx, |state, cx| state.open(other.path().to_owned(), cx));
        cx.executor().run_until_parked();
        assert!(!second.read_with(cx, |state, _| state.is_trusted()));
    }

    fn current_branch(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[gpui::test]
    async fn the_branch_list_loads_on_request(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["branch", "other"]);
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        assert!(state.read_with(cx, |state, _| state.branches().is_none()));

        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            let names: Vec<_> = state
                .branches()
                .unwrap()
                .local
                .iter()
                .map(|b| b.name.clone())
                .collect();
            assert!(names.contains(&"main".to_owned()) && names.contains(&"other".to_owned()));
        });
    }

    #[gpui::test]
    async fn an_untrusted_repository_will_not_switch_or_create_branches(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["branch", "other"]);
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();
        let other = state.read_with(cx, |state, _| {
            state
                .branches()
                .unwrap()
                .local
                .iter()
                .find(|b| b.name == "other")
                .unwrap()
                .clone()
        });

        state.update(cx, |state, cx| state.switch_branch(other, cx));
        cx.executor().run_until_parked();
        assert_eq!(current_branch(dir.path()), "main");
        state.read_with(cx, |state, _| {
            assert!(
                state
                    .error()
                    .unwrap()
                    .contains("Trust this repository to switch branches")
            );
        });

        state.update(cx, |state, cx| state.create_branch("new-one".into(), cx));
        cx.executor().run_until_parked();
        assert_eq!(current_branch(dir.path()), "main");
        state.read_with(cx, |state, _| {
            assert!(
                state
                    .error()
                    .unwrap()
                    .contains("Trust this repository to create a branch")
            );
        });
    }

    #[gpui::test]
    async fn a_trusted_repository_switches_and_creates_branches(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["branch", "other"]);
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();
        let other = state.read_with(cx, |state, _| {
            state
                .branches()
                .unwrap()
                .local
                .iter()
                .find(|b| b.name == "other")
                .unwrap()
                .clone()
        });

        state.update(cx, |state, cx| state.switch_branch(other, cx));
        cx.executor().run_until_parked();
        assert_eq!(current_branch(dir.path()), "other");
        // The status the UI shows follows the checkout.
        state.read_with(cx, |state, _| {
            assert_eq!(
                state.status().unwrap().branch.head,
                git_core::Head::Branch("other".into())
            );
            assert!(
                state.branches().is_none(),
                "a stale list must not be shown after a switch"
            );
        });

        state.update(cx, |state, cx| state.create_branch("brand-new".into(), cx));
        cx.executor().run_until_parked();
        assert_eq!(current_branch(dir.path()), "brand-new");
    }

    #[gpui::test]
    async fn a_bad_branch_name_reports_git_and_leaves_the_branch_alone(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));

        state.update(cx, |state, cx| {
            state.create_branch("not a valid name".into(), cx)
        });
        cx.executor().run_until_parked();

        assert_eq!(current_branch(dir.path()), "main");
        state.read_with(cx, |state, _| {
            assert!(state.error().unwrap().contains("not a valid branch name"));
        });
    }
}
