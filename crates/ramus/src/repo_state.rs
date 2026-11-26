use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use crate::settings::{self, Appearance, RecentRepository, Settings};

use git_core::{
    Branch, BranchKind, BranchList, CancelToken, CommitInfo, DiffHunk, DiscardBackup, FileChange,
    Head, InProgress, Progress, Repo, RepoStatus, Stash, Worktree, worktree_folder_name,
};
use gpui::{Context, EventEmitter, SharedString, Task};

/// A way of exchanging commits with the remote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    Fetch,
    Pull,
    Push,
    /// Push a branch for the first time and track it.
    Publish,
    /// Pull, merging if the branches have diverged.
    PullMerge,
    /// Download a repository for the first time.
    Clone,
}

impl SyncKind {
    /// "Pushing", for a button that shows what is happening.
    pub fn progressive(self) -> &'static str {
        match self {
            Self::Fetch => "Fetching",
            Self::Pull => "Pulling",
            Self::Push => "Pushing",
            Self::Publish => "Publishing",
            Self::PullMerge => "Merging",
            Self::Clone => "Cloning",
        }
    }
}

/// A fetch, pull or push that is running.
pub struct Operation {
    pub kind: SyncKind,
    pub progress: Option<Progress>,
    pub cancelling: bool,
    cancel: CancelToken,
}

/// Things views react to that aren't just "state changed".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoEvent {
    /// A commit succeeded, so the commit message can be cleared.
    Committed,
    /// The last commit was taken back; its message goes back in the commit boxes.
    CommitUndone {
        summary: String,
        description: String,
    },
}

/// The open repository and its latest status, shared by every view.
///
/// Git runs on a background executor; views only ever read the last result.
pub struct RepoState {
    repo: Option<Repo>,
    status: Option<RepoStatus>,
    /// Names of the open repository's remotes; empty means it is local only.
    remotes: Vec<String>,
    /// A merge, cherry-pick or revert that stopped and is waiting to be finished.
    in_progress: Option<InProgress>,
    error: Option<SharedString>,
    /// Bumped on every load so a slow, older load can't overwrite a newer one.
    generation: u64,
    load: Option<Task<()>>,
    /// A folder is being opened. Until it lands nothing else may start: a refresh would
    /// cancel it, and a write would run in the folder the window is leaving.
    opening: bool,
    /// A write (stage, unstage, commit) is running. Writes are never cancelled
    /// by a refresh, and only one runs at a time.
    action: Option<Task<()>>,
    settings: Settings,
    /// Where `settings` is saved, or `None` to keep them in memory only (tests).
    settings_path: Option<PathBuf>,
    /// The open repository may run programs its own config names (hooks, credential
    /// helpers). Until it is trusted, commit and network operations are refused.
    trusted: bool,
    /// Every worktree of the open repository, main first. `None` when git couldn't list
    /// them, or doesn't list the open folder among them
    /// (a submodule, or a folder that only points at another repository's git folder).
    worktrees: Option<Vec<Worktree>>,
    /// Which of `worktrees` is open in the window.
    open_worktree: Option<usize>,
    /// The git folder the open repository's worktrees share, which trust is recorded by.
    /// Only set when the open folder is one of `worktrees`: a folder that merely points
    /// at another repository's git folder must not borrow that repository's trust.
    common_dir: Option<PathBuf>,
    worktrees_task: Option<Task<()>>,
    /// Branches for the branch switcher, loaded when it opens.
    branches: Option<BranchList>,
    branches_task: Option<Task<()>>,
    /// Stashes for the Stashes tab; `None` until loaded.
    stashes: Option<Vec<Stash>>,
    stashes_task: Option<Task<()>>,
    operation: Option<Operation>,
    /// The last fetch, pull or push that was started, so a failure can be retried.
    last_sync: Option<SyncKind>,
    /// A merge-pull is running only so that a push can follow it.
    push_after_pull: bool,
    /// The most recent discard, kept so it can be undone, with a line saying what it was.
    last_discard: Option<(DiscardBackup, String)>,
    /// The commit HEAD was at when `last_discard` was made. Undo only makes sense there.
    discard_head: Option<String>,
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
            remotes: Vec::new(),
            in_progress: None,
            error: None,
            generation: 0,
            load: None,
            opening: false,
            action: None,
            settings,
            settings_path,
            trusted: false,
            worktrees: None,
            open_worktree: None,
            common_dir: None,
            worktrees_task: None,
            branches: None,
            branches_task: None,
            stashes: None,
            stashes_task: None,
            operation: None,
            last_sync: None,
            push_after_pull: false,
            last_discard: None,
            discard_head: None,
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
        self.settings
            .trust(self.common_dir.as_deref().unwrap_or(repo.root()));
        self.trusted = true;
        self.save_settings("the trust setting");
        cx.notify();
    }

    /// The theme the user picked: light, dark, or whatever macOS is using.
    pub fn appearance(&self) -> Appearance {
        self.settings.appearance
    }

    /// Remembers the user's theme choice.
    pub fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.settings.appearance = appearance;
        self.save_settings("the appearance setting");
        cx.notify();
    }

    pub fn last_discard(&self) -> Option<&str> {
        self.last_discard
            .as_ref()
            .map(|(_, message)| message.as_str())
    }

    pub fn dismiss_discard(&mut self, cx: &mut Context<Self>) {
        self.last_discard = None;
        cx.notify();
    }

    /// Throws away the changes to `files`, after saving them so [`undo_discard`] can
    /// bring them back.
    pub fn discard(&mut self, files: Vec<FileChange>, cx: &mut Context<Self>) {
        let message = match files.as_slice() {
            [one] => format!("Discarded changes to {}", one.path),
            many => format!("Discarded changes to {} files", many.len()),
        };
        self.last_discard = None;
        self.write_returning(
            cx,
            move |repo| repo.discard(&files.iter().collect::<Vec<_>>()),
            move |this, backup, _| {
                this.discard_head = this.status.as_ref().and_then(|s| s.branch.oid.clone());
                this.last_discard = Some((backup, message));
            },
        );
    }

    pub fn undo_discard(&mut self, cx: &mut Context<Self>) {
        let Some((backup, _)) = self.last_discard.take() else {
            return;
        };
        self.write(cx, None, move |repo| repo.undo_discard(&backup));
    }

    pub fn operation(&self) -> Option<&Operation> {
        self.operation.as_ref()
    }

    /// Runs a fetch, pull or push in the background, reporting progress as it goes.
    /// Refused in an untrusted repository (hooks and credential helpers would run) and
    /// while another write is running.
    pub fn sync(&mut self, kind: SyncKind, cx: &mut Context<Self>) {
        self.start_sync(kind, false, cx);
    }

    /// A background fetch for the periodic refresh. Does nothing unless it is safe and
    /// useful (trusted, has a remote, nothing else running), and a failure is swallowed:
    /// nobody asked, so a flaky network shouldn't raise a banner every few minutes.
    pub fn auto_fetch(&mut self, cx: &mut Context<Self>) {
        if self.repo.is_none() || !self.trusted || !self.has_remote() || self.occupied() {
            return;
        }
        self.start_sync(SyncKind::Fetch, true, cx);
    }

    fn start_sync(&mut self, kind: SyncKind, quiet: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.occupied() || !self.require_trust("fetch, push or pull", cx) {
            return;
        }
        if !quiet {
            self.last_sync = Some(kind);
        }
        self.run_cancellable(
            kind,
            move |cancel, report| match kind {
                SyncKind::Fetch => repo.fetch(cancel, report),
                SyncKind::Pull => repo.pull(cancel, report),
                SyncKind::Push => repo.push(cancel, report),
                SyncKind::Publish => repo.publish(cancel, report),
                SyncKind::PullMerge => repo.pull_merge(cancel, report),
                SyncKind::Clone => Err(git_core::Error::Parse(
                    "cloning is started from the Clone dialog".into(),
                )),
            },
            move |this, result, cx| {
                let succeeded = result.is_ok();
                match result {
                    // A silent fetch must not wipe an error the user hasn't seen yet.
                    Ok(()) if quiet => {}
                    Ok(()) => this.error = None,
                    // The user asked for this, so it isn't an error to report.
                    Err(git_core::Error::Cancelled | git_core::Error::Conflicts(_)) => {}
                    Err(_) if quiet => {}
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                // New remote branches and counts: the lists and status are stale.
                this.branches = None;
                this.refresh(cx);
                if std::mem::take(&mut this.push_after_pull) && succeeded {
                    this.sync(SyncKind::Push, cx);
                }
            },
            cx,
        );
    }

    /// Whether a failed fetch, pull or push can be tried again.
    pub fn can_retry(&self) -> bool {
        self.last_sync.is_some() && self.operation.is_none()
    }

    /// Repeats the last fetch, pull or push, after it failed.
    pub fn retry_sync(&mut self, cx: &mut Context<Self>) {
        if let Some(kind) = self.last_sync {
            self.error = None;
            self.sync(kind, cx);
        }
    }

    /// The fix for a rejected push: bring in what the remote has (merging if the
    /// branches diverged), then push. If the merge conflicts, it is backed out and the
    /// push does not happen.
    pub fn pull_then_push(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.sync(SyncKind::PullMerge, cx);
        // `sync` declines (untrusted repo, busy) without starting anything.
        self.push_after_pull = self.operation.is_some();
    }

    /// Clones `url` into `destination` and opens it. A repository the user chose to
    /// clone is trusted from the start: they asked for it, and its config is not cloned.
    pub fn clone_repository(&mut self, url: String, destination: PathBuf, cx: &mut Context<Self>) {
        if self.occupied() {
            return;
        }
        self.run_cancellable(
            SyncKind::Clone,
            move |cancel, report| {
                let repo = Repo::clone_from(&url, &destination, cancel, report)?;
                // The clone itself worked; without its git folder it just opens untrusted.
                let common_dir = repo.common_dir().ok();
                Ok((repo, common_dir))
            },
            |this, result, cx| match result {
                Ok((repo, common_dir)) => {
                    let root = repo.root().to_owned();
                    if let Some(common_dir) = &common_dir {
                        this.settings.trust(common_dir);
                    }
                    if let Some(path) = &this.settings_path
                        && let Err(error) = this.settings.save(path)
                    {
                        this.error =
                            Some(format!("Couldn't save the trust setting: {error}").into());
                    }
                    this.open(root, cx);
                }
                Err(git_core::Error::Cancelled) => {}
                Err(error) => this.error = Some(error.to_string().into()),
            },
            cx,
        );
    }

    /// Runs `work` on a background thread as the current cancellable operation, showing
    /// its progress, then hands the result to `finish` on the main thread.
    fn run_cancellable<T: Send + 'static>(
        &mut self,
        kind: SyncKind,
        work: impl FnOnce(
            &CancelToken,
            &mut dyn FnMut(git_core::Progress),
        ) -> Result<T, git_core::Error>
        + Send
        + 'static,
        finish: impl FnOnce(&mut Self, Result<T, git_core::Error>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let cancel = CancelToken::new();
        let (progress_sender, progress_receiver) = mpsc::channel();
        let finished: Arc<Mutex<Option<Result<T, git_core::Error>>>> = Arc::default();

        let work = {
            let (cancel, finished) = (cancel.clone(), finished.clone());
            cx.background_executor().spawn(async move {
                let mut report = |progress| {
                    let _ = progress_sender.send(progress);
                };
                let result = work(&cancel, &mut report);
                *finished.lock().expect("result slot") = Some(result);
            })
        };

        self.operation = Some(Operation {
            kind,
            progress: None,
            cancelling: false,
            cancel,
        });
        self.action = Some(cx.spawn(async move |this, cx| {
            // Progress arrives from another thread, so look for it a few times a second.
            let result = loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let latest = progress_receiver.try_iter().last();
                let done = finished.lock().expect("result slot").take();
                this.update(cx, |this, cx| {
                    if let (Some(progress), Some(operation)) = (latest, this.operation.as_mut()) {
                        operation.progress = Some(progress);
                        cx.notify();
                    }
                })
                .ok();
                if let Some(result) = done {
                    break result;
                }
            };
            work.await;

            this.update(cx, |this, cx| {
                this.action = None;
                this.operation = None;
                finish(this, result, cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Asks the running operation to stop. It ends as soon as git and what it started exit.
    pub fn cancel_sync(&mut self, cx: &mut Context<Self>) {
        if let Some(operation) = self.operation.as_mut() {
            operation.cancel.cancel();
            operation.cancelling = true;
            cx.notify();
        }
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

    pub fn stashes(&self) -> Option<&[Stash]> {
        self.stashes.as_deref()
    }

    /// Reads the stash list in the background.
    pub fn load_stashes(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.stashes_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.stashes() })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(stashes) => this.stashes = Some(stashes),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Sets every change aside under `message` (git picks one if it is empty).
    pub fn stash_changes(&mut self, message: String, cx: &mut Context<Self>) {
        self.write_returning(
            cx,
            move |repo| repo.stash_push(&message),
            |this, (), cx| this.load_stashes(cx),
        );
    }

    /// Brings a stash back and keeps it. Applying can run the repository's merge
    /// drivers, so it needs trust.
    pub fn apply_stash(&mut self, stash: Stash, cx: &mut Context<Self>) {
        if self.require_trust("apply a stash", cx) {
            self.write_returning(
                cx,
                move |repo| repo.stash_apply(&stash),
                |this, (), cx| this.load_stashes(cx),
            );
        }
    }

    /// Brings a stash back and removes it.
    pub fn pop_stash(&mut self, stash: Stash, cx: &mut Context<Self>) {
        if self.require_trust("apply a stash", cx) {
            self.write_returning(
                cx,
                move |repo| repo.stash_pop(&stash),
                |this, (), cx| this.load_stashes(cx),
            );
        }
    }

    pub fn drop_stash(&mut self, stash: Stash, cx: &mut Context<Self>) {
        self.write_returning(
            cx,
            move |repo| repo.stash_drop(&stash),
            |this, (), cx| this.load_stashes(cx),
        );
    }

    /// Makes a worktree on a new branch off the default branch, in a folder next to the
    /// main one. Creating it runs the repository's hooks, so it needs trust.
    pub fn add_worktree(&mut self, branch: String, cx: &mut Context<Self>) {
        if self.require_trust("create a worktree", cx) {
            self.write_returning(
                cx,
                // Even a failure can leave a worktree behind (a failing hook), so the
                // outcome is carried through and the list is read again either way.
                move |repo| {
                    Ok(repo
                        .add_worktree(&branch, &worktree_folder_name(&branch))
                        .map(drop))
                },
                |this, created, cx| {
                    if let Err(error) = created {
                        this.error = Some(error.to_string().into());
                    }
                    this.load_worktrees(cx);
                },
            );
        }
    }

    /// Removes a linked worktree, and with `delete_branch` the branch it had checked out.
    /// `force` discards its uncommitted changes. If the open worktree is the one removed,
    /// the main worktree is opened instead.
    pub fn remove_worktree(
        &mut self,
        worktree: Worktree,
        force: bool,
        delete_branch: bool,
        cx: &mut Context<Self>,
    ) {
        let was_open = self.is_open_worktree(&worktree);
        self.write_returning(
            cx,
            move |repo| {
                let main = repo.remove_worktree(&worktree, force)?;
                // The worktree is gone by now, so a branch that won't delete is reported
                // without losing the main worktree to fall back to.
                let branch_deleted = match (delete_branch, &worktree.head) {
                    (true, Head::Branch(name)) => main.delete_branch(name),
                    _ => Ok(()),
                };
                Ok((main, branch_deleted))
            },
            move |this, (main, branch_deleted), cx| {
                if was_open {
                    // Opened without clearing the error, so a branch that wouldn't
                    // delete is still reported once the main worktree is showing.
                    this.load(None, main.root().to_owned(), false, cx);
                } else {
                    this.load_worktrees(cx);
                }
                if let Err(error) = branch_deleted {
                    this.error = Some(error.to_string().into());
                }
            },
        );
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

    /// The merge, cherry-pick or revert waiting for its conflicts to be dealt with.
    pub fn in_progress(&self) -> Option<&InProgress> {
        self.in_progress.as_ref()
    }

    /// Paths in conflict right now.
    pub fn conflicted_paths(&self) -> Vec<String> {
        self.status
            .as_ref()
            .map(|status| {
                status
                    .files
                    .iter()
                    .filter(|file| file.kind() == git_core::ChangeKind::Conflicted)
                    .map(|file| file.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Saves `text` as the resolution of `path` and stages it.
    pub fn resolve_with_text(&mut self, path: String, text: String, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| repo.resolve_with_text(&path, &text));
    }

    pub fn resolve_with_side(
        &mut self,
        path: String,
        side: git_core::Side,
        cx: &mut Context<Self>,
    ) {
        self.write(cx, None, move |repo| repo.resolve_with_side(&path, side));
    }

    pub fn resolve_keep(&mut self, path: String, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| repo.resolve_keep(&path));
    }

    pub fn resolve_delete(&mut self, path: String, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| repo.resolve_delete(&path));
    }

    /// Gives up on the stopped operation, putting everything back as it was.
    pub fn abort_operation(&mut self, cx: &mut Context<Self>) {
        self.write(cx, None, |repo| repo.abort_operation());
    }

    /// Finishes the stopped operation once every conflict is resolved. This commits, so
    /// the repository's hooks run and it needs trust. If a pull's merge was waiting on
    /// this, the push that follows is the user's next click.
    pub fn continue_operation(&mut self, cx: &mut Context<Self>) {
        if self.require_trust("finish the merge", cx) {
            self.write(cx, Some(RepoEvent::Committed), |repo| {
                repo.continue_operation()
            });
        }
    }

    /// Whether the open repository has anywhere to fetch from or push to.
    pub fn has_remote(&self) -> bool {
        !self.remotes.is_empty()
    }

    /// Gives a local-only repository an `origin`.
    pub fn add_origin(&mut self, url: String, cx: &mut Context<Self>) {
        self.write(cx, None, move |repo| repo.add_origin(&url));
    }

    /// Checks out `branch`, or, when another worktree has it checked out (git allows a
    /// branch in only one worktree), opens that worktree instead.
    pub fn switch_branch(&mut self, branch: Branch, cx: &mut Context<Self>) {
        if let Some(worktree) = self.worktree_holding(&branch) {
            self.switch_worktree(&worktree, cx);
            return;
        }
        if self.require_trust("switch branches", cx) {
            self.branches = None;
            self.write(cx, None, move |repo| repo.switch_branch(&branch));
        }
    }

    /// Adds a commit that undoes `commit`. A conflict aborts it, leaving nothing behind.
    pub fn revert_commit(&mut self, commit: CommitInfo, cx: &mut Context<Self>) {
        if self.require_trust("revert a commit", cx) {
            self.write(cx, None, move |repo| repo.revert(&commit));
        }
    }

    /// Applies `commit` on top of the current branch. A conflict aborts it.
    pub fn cherry_pick_commit(&mut self, commit: CommitInfo, cx: &mut Context<Self>) {
        if self.require_trust("cherry-pick a commit", cx) {
            self.write(cx, None, move |repo| repo.cherry_pick(&commit));
        }
    }

    /// Merges `branch` into the current branch. A conflict aborts it.
    pub fn merge_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        if self.require_trust("merge a branch", cx) {
            self.branches = None;
            self.write(cx, None, move |repo| repo.merge(&branch));
        }
    }

    /// Renames local branch `old`. A ref write with hooks off, so it is allowed before the
    /// repository is trusted.
    pub fn rename_branch(&mut self, old: String, new: String, cx: &mut Context<Self>) {
        self.write_returning(
            cx,
            move |repo| repo.rename_branch(&old, &new),
            |this, (), cx| this.load_branches(cx),
        );
    }

    /// Deletes local branch `name`, unmerged commits and all; the caller asks first.
    pub fn delete_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.write_returning(
            cx,
            move |repo| repo.delete_branch(&name),
            |this, (), cx| this.load_branches(cx),
        );
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

    /// Whether another folder can be opened now: nothing is running and no folder is
    /// still opening.
    pub fn can_switch(&self) -> bool {
        !self.occupied()
    }

    /// Whether new work must wait: a write is running, or a folder is still opening.
    fn occupied(&self) -> bool {
        self.action.is_some() || self.opening
    }

    /// Shows `message` in the error banner, for a failure outside the state's own work.
    pub fn report(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// Every worktree of the open repository, main first. `None` before a load, and
    /// when git couldn't list them.
    pub fn worktrees(&self) -> Option<&[Worktree]> {
        self.worktrees.as_deref()
    }

    /// The repository's name: the main worktree's folder name, falling back to the open
    /// folder's name.
    pub fn repository_name(&self) -> Option<String> {
        let main = self
            .worktrees()
            .and_then(|worktrees| worktrees.iter().find(|worktree| worktree.is_main))
            .map(|worktree| worktree.path.as_path());
        let folder = main.or(self.repo.as_ref().map(Repo::root))?;
        Some(folder.file_name()?.to_string_lossy().into_owned())
    }

    /// Re-reads the worktree list in the background, for when the list is about to be shown.
    pub fn load_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.worktrees_task = Some(cx.spawn(async move |this, cx| {
            let (repo, listed) = cx
                .background_executor()
                .spawn(async move {
                    let listed = listed_worktrees(&repo);
                    (repo, listed)
                })
                .await;
            this.update(cx, |this, cx| {
                let still_open = this.repo.as_ref().map(Repo::root) == Some(repo.root());
                // A list that couldn't be read leaves the last good one showing.
                if let (true, Some((worktrees, open))) = (still_open, listed) {
                    this.worktrees = Some(worktrees);
                    this.open_worktree = Some(open);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Whether `worktree` is the one open in the window.
    pub fn is_open_worktree(&self, worktree: &Worktree) -> bool {
        let open = self
            .open_worktree
            .and_then(|index| self.worktrees()?.get(index));
        open.is_some_and(|open| open.path == worktree.path)
    }

    /// The other worktree that has local `branch` checked out, if its folder is still
    /// there. A missing one is left to git, whose refusal names the folder.
    pub fn worktree_holding(&self, branch: &Branch) -> Option<Worktree> {
        if branch.kind != BranchKind::Local {
            return None;
        }
        self.worktrees()?
            .iter()
            .find(|worktree| {
                worktree.head == Head::Branch(branch.name.clone())
                    && !worktree.missing
                    && !self.is_open_worktree(worktree)
            })
            .cloned()
    }

    /// Opens `worktree` in this window. Ignored while a write, a sync or another open is
    /// running, so the running work never reloads a different folder, and for a worktree
    /// whose folder is gone.
    pub fn switch_worktree(&mut self, worktree: &Worktree, cx: &mut Context<Self>) {
        if self.occupied() || worktree.missing {
            return;
        }
        self.open(worktree.path.clone(), cx);
    }

    /// The repositories the user has opened, most recent first.
    pub fn recent_repositories(&self) -> &[RecentRepository] {
        &self.settings.repositories
    }

    /// Whether `entry` is the repository open now, in whichever of its worktrees.
    pub fn is_open_repository(&self, entry: &RecentRepository) -> bool {
        self.repo.as_ref().is_some_and(|repo| {
            settings::identity(self.common_dir.as_deref().unwrap_or(repo.root()))
                == entry.repository
        })
    }

    /// Opens the folder `entry` remembers. Ignored while a write, a sync or another open
    /// is running, as switching worktrees is, and for a folder that is gone.
    pub fn switch_repository(&mut self, entry: &RecentRepository, cx: &mut Context<Self>) {
        if self.occupied() || !entry.open_path.exists() {
            return;
        }
        self.open(entry.open_path.clone(), cx);
    }

    /// Takes `entry` off the list. An open repository stays open, and stays trusted.
    pub fn forget_repository(&mut self, entry: &RecentRepository, cx: &mut Context<Self>) {
        self.settings.forget(&entry.repository);
        self.save_settings("the repository list");
        cx.notify();
    }

    /// Writes the settings file, showing a failure in the banner: the change still holds
    /// for this session, it just won't be remembered.
    fn save_settings(&mut self, what: &str) {
        if let Some(path) = &self.settings_path
            && let Err(error) = self.settings.save(path)
        {
            self.error = Some(format!("Couldn't save {what}: {error}").into());
        }
    }

    /// Opens the repository containing `path`. On failure the current repo stays open.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load(None, path, true, cx);
    }

    /// Re-reads status for the open repository, if any. Skipped while a folder is
    /// opening: that load is about to bring everything up to date.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.opening {
            return;
        }
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
        // Where a folder sits among its repository's worktrees, and so whether it is
        // trusted, is settled when it is opened. Refreshes leave it alone, so one failed
        // read can't drop a trusted repository back to Restricted mode.
        let opening = known.is_none();
        self.opening |= opening;

        self.load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let repo = match known {
                        Some(repo) => repo,
                        None => Repo::open(&path)?,
                    };
                    let status = repo.status()?;
                    let remotes = repo.remotes().unwrap_or_default();
                    let in_progress = repo.in_progress().ok().flatten();
                    let place = opening.then(|| match listed_worktrees(&repo) {
                        Some((worktrees, open)) => {
                            (Some(worktrees), Some(open), repo.common_dir().ok())
                        }
                        None => (None, None, None),
                    });
                    Ok::<_, git_core::Error>((repo, status, remotes, in_progress, place))
                })
                .await;

            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if opening {
                    this.opening = false;
                }
                match result {
                    Ok((repo, status, remotes, in_progress, place)) => {
                        this.remotes = remotes;
                        this.in_progress = in_progress;
                        if this.repo.as_ref().map(Repo::root) != Some(repo.root()) {
                            this.stashes = None;
                            this.branches = None;
                            // The files an undo would restore belong to the other folder.
                            this.last_discard = None;
                        }
                        if let Some(place) = place {
                            (this.worktrees, this.open_worktree, this.common_dir) = place;
                            this.trusted = this.settings.is_trusted(
                                repo.root(),
                                this.common_dir.as_deref().unwrap_or(repo.root()),
                            );
                        }
                        this.repo = Some(repo);
                        // Restoring discarded files into a different commit's tree would be
                        // wrong, so the undo lapses when HEAD moves.
                        let head = status.branch.oid.clone();
                        if this.last_discard.is_some() && head != this.discard_head {
                            this.last_discard = None;
                        }
                        this.status = Some(status);
                        if clear_error {
                            this.error = None;
                        }
                        // A folder that opened goes to the top of the Repositories list.
                        // After the error is cleared, so a failed save still shows.
                        if opening && let Some(repo) = &this.repo {
                            let root = repo.root().to_owned();
                            let common_dir = this.common_dir.clone().unwrap_or(root.clone());
                            this.settings.remember(&common_dir, &root);
                            this.save_settings("the repository list");
                        }
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Replaces the last commit with one that also holds what is staged.
    pub fn amend(&mut self, summary: String, description: String, cx: &mut Context<Self>) {
        // Amending runs the repository's hooks, like any commit.
        if !self.require_trust("amend", cx) {
            return;
        }
        self.write(cx, Some(RepoEvent::Committed), move |repo| {
            repo.amend(&summary, &description)
        });
    }

    /// Takes back the last commit, `shown`, if it isn't pushed: its changes stay staged and
    /// its message is announced so the commit boxes can show it again. A ref write with
    /// hooks off, so it is allowed before the repository is trusted, like stashing.
    pub fn undo_last_commit(&mut self, shown: String, cx: &mut Context<Self>) {
        self.write_returning(
            cx,
            move |repo| repo.undo_last_commit(&shown),
            |_, (summary, description), cx| {
                cx.emit(RepoEvent::CommitUndone {
                    summary,
                    description,
                })
            },
        );
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

    /// Stages just the chosen lines (indexes into `hunk.lines`) of one unstaged hunk.
    pub fn stage_lines(
        &mut self,
        path: String,
        hunk: DiffHunk,
        selected: BTreeSet<usize>,
        cx: &mut Context<Self>,
    ) {
        self.write(cx, None, move |repo| {
            repo.stage_lines(&path, &hunk, &selected)
        });
    }

    /// Unstages just the chosen lines of one staged hunk.
    pub fn unstage_lines(
        &mut self,
        path: String,
        hunk: DiffHunk,
        selected: BTreeSet<usize>,
        cx: &mut Context<Self>,
    ) {
        self.write(cx, None, move |repo| {
            repo.unstage_lines(&path, &hunk, &selected)
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
        self.write_returning(cx, work, move |_, (), cx| {
            if let Some(event) = on_success {
                cx.emit(event);
            }
        });
    }

    /// Like [`write`], for work that produces a value the state wants to keep.
    fn write_returning<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(&Repo) -> Result<T, git_core::Error> + Send + 'static,
        on_success: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.occupied() {
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
                    Ok(value) => {
                        this.error = None;
                        on_success(this, value, cx);
                    }
                    // Stopping for conflicts is a state to resolve, not a failure.
                    Err(git_core::Error::Conflicts(_)) => this.error = None,
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

/// The repository's worktrees and which of them `repo` is. `None` when git can't list
/// them or doesn't list `repo`'s folder among them.
fn listed_worktrees(repo: &Repo) -> Option<(Vec<Worktree>, usize)> {
    let worktrees = repo.worktrees().ok()?;
    let open = worktrees
        .iter()
        .position(|worktree| same_folder(&worktree.path, repo.root()))?;
    Some((worktrees, open))
}

/// Whether two paths are the same folder once symlinks are resolved.
fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn as_strs(paths: &[String]) -> Vec<&str> {
    paths.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use gpui::{AppContext, TestAppContext};
    use tempfile::TempDir;

    use git_core::Head;

    use super::*;
    use crate::testutil::git;

    #[gpui::test]
    fn setting_the_appearance_is_saved(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        let state = cx.new(|_| RepoState::with_settings(Settings::default(), Some(path.clone())));

        state.update(cx, |state, cx| state.set_appearance(Appearance::Dark, cx));

        assert_eq!(
            state.read_with(cx, |state, _| state.appearance()),
            Appearance::Dark
        );
        assert_eq!(Settings::load(&path).appearance, Appearance::Dark);
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

    /// Lets a running sync finish: the test clock doesn't move by itself, so step it
    /// past the progress timer until the operation is over.
    fn settle(cx: &mut TestAppContext, state: &gpui::Entity<RepoState>) {
        for _ in 0..100 {
            cx.executor().advance_clock(Duration::from_millis(150));
            cx.executor().run_until_parked();
            if !state.read_with(cx, |state, _| state.is_busy()) {
                return;
            }
        }
        panic!("the operation never finished");
    }

    /// A bare remote with two clones of its `main`; returns (remote, a, b).
    fn remote_and_two_clones() -> (TempDir, TempDir, TempDir) {
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let seed = repo_without_hooks();
        git(seed.path(), &["add", "-A"]);
        git(seed.path(), &["commit", "-q", "-m", "seed"]);
        git(
            seed.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(seed.path(), &["push", "-q", "origin", "main"]);
        let clone = |remote: &Path| {
            let dir = TempDir::new().unwrap();
            git(dir.path(), &["clone", "-q", remote.to_str().unwrap(), "."]);
            dir
        };
        let (a, b) = (clone(remote.path()), clone(remote.path()));
        (remote, a, b)
    }

    fn open_trusted(cx: &mut TestAppContext, dir: &Path) -> gpui::Entity<RepoState> {
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        state
    }

    #[gpui::test]
    async fn fetching_updates_how_far_behind_the_branch_is(cx: &mut TestAppContext) {
        let (_remote, a, b) = remote_and_two_clones();
        fs::write(b.path().join("news.txt"), "n\n").unwrap();
        git(b.path(), &["add", "."]);
        git(b.path(), &["commit", "-q", "-m", "news"]);
        git(b.path(), &["push", "-q", "origin", "main"]);
        let state = open_trusted(cx, a.path());
        assert_eq!(
            state.read_with(cx, |s, _| s.status().unwrap().branch.behind),
            0
        );

        state.update(cx, |state, cx| state.sync(SyncKind::Fetch, cx));
        assert!(state.read_with(cx, |state, _| state.operation().is_some()));
        settle(cx, &state);

        state.read_with(cx, |state, _| {
            assert_eq!(state.status().unwrap().branch.behind, 1);
            assert!(state.operation().is_none());
            assert!(state.error().is_none());
        });
    }

    #[gpui::test]
    async fn a_cloned_repository_opens_and_is_trusted(cx: &mut TestAppContext) {
        let (remote, _a, _b) = remote_and_two_clones();
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("copy");
        let state = cx.new(|_| RepoState::default());

        state.update(cx, |state, cx| {
            state.clone_repository(
                remote.path().to_str().unwrap().to_owned(),
                destination.clone(),
                cx,
            )
        });
        assert_eq!(
            state.read_with(cx, |s, _| s.operation().map(|o| o.kind)),
            Some(SyncKind::Clone)
        );
        settle(cx, &state);

        state.read_with(cx, |state, _| {
            assert!(state.error().is_none(), "{:?}", state.error());
            assert!(state.operation().is_none());
            assert_eq!(
                state.repo().unwrap().root().canonicalize().unwrap(),
                destination.canonicalize().unwrap()
            );
            assert!(state.is_trusted());
        });
    }

    #[gpui::test]
    async fn a_clone_is_remembered_by_its_shared_git_folder(cx: &mut TestAppContext) {
        let (remote, _a, _b) = remote_and_two_clones();
        let parent = TempDir::new().unwrap();
        let destination = parent.path().join("copy");
        let state = cx.new(|_| RepoState::default());

        state.update(cx, |state, cx| {
            state.clone_repository(
                remote.path().to_str().unwrap().to_owned(),
                destination.clone(),
                cx,
            )
        });
        settle(cx, &state);

        let shared_git_folder = destination.join(".git").canonicalize().unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(
                state.settings.trusted_repositories,
                BTreeSet::from([shared_git_folder])
            );
        });
    }

    /// A repository at `<parent>/main` with a second worktree at `<parent>/linked` on the
    /// branch `feature`; returns (parent, main, linked).
    fn repo_with_linked_worktree() -> (TempDir, PathBuf, PathBuf) {
        let parent = TempDir::new().unwrap();
        let main = parent.path().join("main");
        let linked = parent.path().join("linked");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        fs::write(main.join("a.txt"), "a\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "base"]);
        git(
            &main,
            &["worktree", "add", "-q", "-b", "feature", "../linked"],
        );
        (parent, main, linked)
    }

    fn open_root(cx: &mut TestAppContext, state: &gpui::Entity<RepoState>) -> PathBuf {
        state.read_with(cx, |state, _| {
            state.repo().unwrap().root().canonicalize().unwrap()
        })
    }

    fn worktree_at(
        cx: &mut TestAppContext,
        state: &gpui::Entity<RepoState>,
        index: usize,
    ) -> Worktree {
        state.read_with(cx, |state, _| state.worktrees().unwrap()[index].clone())
    }

    #[gpui::test]
    async fn the_worktree_list_loads_with_the_repository(cx: &mut TestAppContext) {
        let (_parent, main, _linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());

        state.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            let worktrees = state.worktrees().unwrap();
            assert_eq!(worktrees.len(), 2);
            assert!(worktrees[0].is_main);
            assert!(state.is_open_worktree(&worktrees[0]));
            assert!(!state.is_open_worktree(&worktrees[1]));
        });
    }

    #[gpui::test]
    async fn switching_worktrees_shows_that_worktree_under_the_repository_name(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, _linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();
        let linked = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| state.switch_worktree(&linked, cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(
                state.status().unwrap().branch.head,
                Head::Branch("feature".into())
            );
            assert_eq!(state.repository_name(), Some("main".to_owned()));
            assert!(state.is_open_worktree(&linked));
        });
    }

    #[gpui::test]
    async fn removing_the_open_worktree_opens_the_main_one_still_trusted(cx: &mut TestAppContext) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let state = open_trusted(cx, &linked);
        let open = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| state.remove_worktree(open, false, true, cx));
        cx.executor().run_until_parked();

        assert!(!linked.exists());
        assert_eq!(open_root(cx, &state), main.canonicalize().unwrap());
        state.read_with(cx, |state, _| {
            assert!(state.error().is_none(), "{:?}", state.error());
            assert!(state.is_trusted());
            let worktrees = state.worktrees().unwrap();
            assert_eq!(worktrees.len(), 1);
            assert!(state.is_open_worktree(&worktrees[0]));
        });
        assert!(!crate::testutil::git_ok(
            &main,
            &["rev-parse", "--verify", "-q", "refs/heads/feature"]
        ));
    }

    #[gpui::test]
    async fn a_branch_that_will_not_delete_is_reported_after_its_open_worktree_is_removed(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let state = open_trusted(cx, &linked);
        let open = worktree_at(cx, &state, 1);
        // The branch the list remembers no longer goes by that name.
        git(&main, &["branch", "-m", "feature", "renamed"]);

        state.update(cx, |state, cx| state.remove_worktree(open, false, true, cx));
        cx.executor().run_until_parked();

        assert!(!linked.exists());
        assert_eq!(open_root(cx, &state), main.canonicalize().unwrap());
        assert!(state.read_with(cx, |state, _| state.error().is_some()));
    }

    #[gpui::test]
    async fn a_worktree_left_behind_by_a_failing_hook_is_listed_with_the_error(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, _linked) = repo_with_linked_worktree();
        let script = main.join(".git/hooks/post-checkout");
        fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let state = open_trusted(cx, &main);

        state.update(cx, |state, cx| state.add_worktree("topic".into(), cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            assert!(state.error().is_some());
            assert_eq!(state.worktrees().unwrap().len(), 3);
        });
    }

    #[gpui::test]
    async fn trusting_a_linked_worktree_trusts_the_whole_repository(cx: &mut TestAppContext) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let settings_dir = TempDir::new().unwrap();
        let settings_path = settings_dir.path().join("settings.json");
        let first =
            cx.new(|_| RepoState::with_settings(Settings::default(), Some(settings_path.clone())));
        first.update(cx, |state, cx| state.open(linked.clone(), cx));
        cx.executor().run_until_parked();

        first.update(cx, |state, cx| state.trust(cx));

        first.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();
        assert!(first.read_with(cx, |state, _| state.is_trusted()));
        // A new session reads the same file.
        let second = cx.new(|_| {
            RepoState::with_settings(Settings::load(&settings_path), Some(settings_path.clone()))
        });
        second.update(cx, |state, cx| state.open(linked, cx));
        cx.executor().run_until_parked();
        assert!(second.read_with(cx, |state, _| state.is_trusted()));
    }

    #[gpui::test]
    async fn a_folder_that_only_points_at_a_trusted_repository_is_not_trusted(
        cx: &mut TestAppContext,
    ) {
        let (parent, main, _linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main.clone(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        // A folder nobody added as a worktree, whose `.git` claims the trusted
        // repository's git folder as its own.
        let impostor = parent.path().join("impostor");
        fs::create_dir(&impostor).unwrap();
        git(&impostor, &["init", "-q", "-b", "main"]);
        fs::write(
            impostor.join(".git/commondir"),
            format!("{}\n", main.join(".git").display()),
        )
        .unwrap();

        state.update(cx, |state, cx| state.open(impostor.clone(), cx));
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), impostor.canonicalize().unwrap());
        assert!(!state.read_with(cx, |state, _| state.is_trusted()));
    }

    #[gpui::test]
    async fn a_folder_git_does_not_list_as_a_worktree_goes_by_its_own_name(
        cx: &mut TestAppContext,
    ) {
        // Asking to trust it under the real repository's name would mislead.
        let (parent, main, _linked) = repo_with_linked_worktree();
        let impostor = parent.path().join("impostor");
        fs::create_dir(&impostor).unwrap();
        git(&impostor, &["init", "-q", "-b", "main"]);
        fs::write(
            impostor.join(".git/commondir"),
            format!("{}\n", main.join(".git").display()),
        )
        .unwrap();
        let state = cx.new(|_| RepoState::default());

        state.update(cx, |state, cx| state.open(impostor, cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.repository_name(), Some("impostor".to_owned()));
            assert!(state.worktrees().is_none());
        });
    }

    #[gpui::test]
    async fn a_refresh_during_a_switch_does_not_cancel_it(cx: &mut TestAppContext) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();
        let target = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| {
            state.switch_worktree(&target, cx);
            state.refresh(cx);
        });
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), linked.canonicalize().unwrap());
    }

    #[gpui::test]
    async fn a_write_cannot_start_while_another_worktree_is_opening(cx: &mut TestAppContext) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        fs::write(main.join("b.txt"), "b\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main.clone(), cx));
        cx.executor().run_until_parked();
        let target = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| {
            state.switch_worktree(&target, cx);
            // Would stage a file in the worktree the window is leaving.
            state.stage(vec!["b.txt".into()], cx);
        });
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), linked.canonicalize().unwrap());
        let staged_in_main = Repo::open(&main).unwrap().status().unwrap();
        assert!(!staged_in_main.files.iter().any(|f| f.is_fully_staged()));
    }

    #[gpui::test]
    async fn a_trusted_repository_stays_trusted_when_a_refresh_cannot_list_worktrees(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        // A worktree on a ref that isn't a branch is one the list can't be read with.
        git(&linked, &["symbolic-ref", "HEAD", "refs/elsewhere/x"]);

        state.update(cx, |state, cx| state.refresh(cx));
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.is_trusted()));
    }

    #[gpui::test]
    async fn reloading_the_worktree_list_picks_up_a_worktree_made_elsewhere(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, _linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main.clone(), cx));
        cx.executor().run_until_parked();
        git(&main, &["worktree", "add", "-q", "-b", "later", "../later"]);

        state.update(cx, |state, cx| state.load_worktrees(cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            let worktrees = state.worktrees().unwrap();
            assert_eq!(worktrees.len(), 3);
            assert!(state.is_open_worktree(&worktrees[0]));
        });
    }

    #[gpui::test]
    async fn the_branch_list_of_the_previous_worktree_is_not_kept_after_switching(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, _linked) = repo_with_linked_worktree();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();
        assert!(state.read_with(cx, |state, _| state.branches().is_some()));
        let target = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| state.switch_worktree(&target, cx));
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.branches().is_none()));
    }

    /// A bare repository at `<parent>/bare.git` with checkouts `<parent>/one` and
    /// `<parent>/two`; returns (parent, one, two).
    fn bare_repo_with_two_checkouts() -> (TempDir, PathBuf, PathBuf) {
        let (parent, main, _linked) = repo_with_linked_worktree();
        let bare = parent.path().join("bare.git");
        git(
            parent.path(),
            &[
                "clone",
                "-q",
                "--bare",
                main.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        git(&bare, &["worktree", "add", "-q", "../one", "main"]);
        git(&bare, &["worktree", "add", "-q", "-b", "second", "../two"]);
        let (one, two) = (parent.path().join("one"), parent.path().join("two"));
        (parent, one, two)
    }

    #[gpui::test]
    async fn a_checkout_of_a_bare_repository_lists_the_other_checkouts(cx: &mut TestAppContext) {
        let (_parent, one, _two) = bare_repo_with_two_checkouts();
        let state = cx.new(|_| RepoState::default());

        state.update(cx, |state, cx| state.open(one.clone(), cx));
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), one.canonicalize().unwrap());
        state.read_with(cx, |state, _| {
            assert!(state.error().is_none(), "{:?}", state.error());
            let worktrees = state.worktrees().unwrap();
            assert_eq!(worktrees.len(), 2);
            assert!(worktrees.iter().all(|worktree| !worktree.is_main));
            assert!(state.is_open_worktree(&worktrees[0]));
            // There is no main checkout to name the repository after.
            assert_eq!(state.repository_name(), Some("one".to_owned()));
            assert!(!state.is_trusted());
        });
    }

    #[gpui::test]
    async fn trusting_one_checkout_of_a_bare_repository_trusts_the_others(cx: &mut TestAppContext) {
        let (_parent, one, two) = bare_repo_with_two_checkouts();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(one, cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.trust(cx));
        state.update(cx, |state, cx| state.open(two, cx));
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.is_trusted()));
    }

    #[gpui::test]
    async fn switching_worktrees_while_busy_is_ignored(cx: &mut TestAppContext) {
        let (_parent, main, _linked) = repo_with_linked_worktree();
        fs::write(main.join("b.txt"), "b\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main.clone(), cx));
        cx.executor().run_until_parked();
        let linked = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| {
            state.stage(vec!["b.txt".into()], cx);
            assert!(state.is_busy());
            let loads_before = state.generation;
            state.switch_worktree(&linked, cx);
            // The write reloads when it finishes, which would hide a switch that had
            // started; so check here that none did.
            assert_eq!(state.generation, loads_before);
        });
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), main.canonicalize().unwrap());
    }

    /// State that saves its settings to a file in a fresh folder, returned with it.
    fn state_saving_settings(
        cx: &mut TestAppContext,
    ) -> (TempDir, PathBuf, gpui::Entity<RepoState>) {
        let settings_dir = TempDir::new().unwrap();
        let settings_path = settings_dir.path().join("settings.json");
        let state =
            cx.new(|_| RepoState::with_settings(Settings::default(), Some(settings_path.clone())));
        (settings_dir, settings_path, state)
    }

    fn listed(cx: &mut TestAppContext, state: &gpui::Entity<RepoState>) -> Vec<PathBuf> {
        state.read_with(cx, |state, _| {
            state
                .recent_repositories()
                .iter()
                .map(|entry| entry.open_path.canonicalize().unwrap())
                .collect()
        })
    }

    fn open_folder(cx: &mut TestAppContext, state: &gpui::Entity<RepoState>, path: &Path) {
        state.update(cx, |state, cx| state.open(path.to_owned(), cx));
        cx.executor().run_until_parked();
    }

    #[gpui::test]
    async fn opening_a_repository_puts_it_on_the_list_and_saves_it(cx: &mut TestAppContext) {
        let (_settings_dir, settings_path, state) = state_saving_settings(cx);
        let repo = repo_without_hooks();

        open_folder(cx, &state, repo.path());

        assert_eq!(listed(cx, &state), [repo.path().canonicalize().unwrap()]);
        assert_eq!(Settings::load(&settings_path).repositories.len(), 1);
    }

    #[gpui::test]
    async fn a_folder_that_is_not_a_repository_is_not_listed(cx: &mut TestAppContext) {
        let (_settings_dir, _settings_path, state) = state_saving_settings(cx);
        let folder = TempDir::new().unwrap();

        open_folder(cx, &state, folder.path());

        assert!(listed(cx, &state).is_empty());
        assert!(state.read_with(cx, |state, _| state.error().is_some()));
    }

    #[gpui::test]
    async fn switching_worktrees_updates_the_entry_instead_of_adding_one(cx: &mut TestAppContext) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let (_settings_dir, _settings_path, state) = state_saving_settings(cx);
        open_folder(cx, &state, &main);

        let worktree = worktree_at(cx, &state, 1);
        state.update(cx, |state, cx| state.switch_worktree(&worktree, cx));
        cx.executor().run_until_parked();

        assert_eq!(listed(cx, &state), [linked.canonicalize().unwrap()]);
    }

    #[gpui::test]
    async fn refreshing_does_not_reorder_the_list(cx: &mut TestAppContext) {
        let (_settings_dir, settings_path, state) = state_saving_settings(cx);
        let (a, b) = (repo_without_hooks(), repo_without_hooks());
        open_folder(cx, &state, a.path());
        open_folder(cx, &state, b.path());
        fs::remove_file(&settings_path).unwrap();

        state.update(cx, |state, cx| state.refresh(cx));
        cx.executor().run_until_parked();

        assert_eq!(
            listed(cx, &state),
            [
                b.path().canonicalize().unwrap(),
                a.path().canonicalize().unwrap()
            ]
        );
        assert!(
            !settings_path.exists(),
            "a refresh shouldn't rewrite settings"
        );
    }

    #[gpui::test]
    async fn switching_repository_while_busy_is_ignored(cx: &mut TestAppContext) {
        let (_settings_dir, _settings_path, state) = state_saving_settings(cx);
        let (a, b) = (repo_without_hooks(), repo_without_hooks());
        open_folder(cx, &state, a.path());
        open_folder(cx, &state, b.path());
        let a_entry = state.read_with(cx, |state, _| state.recent_repositories()[1].clone());

        state.update(cx, |state, cx| {
            state.stage(vec!["b.txt".into()], cx);
            assert!(state.is_busy());
            let loads_before = state.generation;
            state.switch_repository(&a_entry, cx);
            assert_eq!(state.generation, loads_before);
        });
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), b.path().canonicalize().unwrap());
    }

    #[gpui::test]
    async fn no_switch_is_possible_while_a_folder_is_still_opening(cx: &mut TestAppContext) {
        let (_settings_dir, _settings_path, state) = state_saving_settings(cx);
        let (a, b) = (repo_without_hooks(), repo_without_hooks());
        open_folder(cx, &state, a.path());
        assert!(state.read_with(cx, |state, _| state.can_switch()));

        state.update(cx, |state, cx| {
            state.open(b.path().to_owned(), cx);
            assert!(!state.can_switch());
        });
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.can_switch()));
    }

    #[gpui::test]
    async fn switching_repository_opens_the_folder_it_remembers(cx: &mut TestAppContext) {
        let (_settings_dir, _settings_path, state) = state_saving_settings(cx);
        let (a, b) = (repo_without_hooks(), repo_without_hooks());
        open_folder(cx, &state, a.path());
        open_folder(cx, &state, b.path());
        let a_entry = state.read_with(cx, |state, _| state.recent_repositories()[1].clone());

        state.update(cx, |state, cx| state.switch_repository(&a_entry, cx));
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), a.path().canonicalize().unwrap());
        assert!(state.read_with(cx, |state, _| state.is_open_repository(&a_entry)));
    }

    #[gpui::test]
    async fn forgetting_the_open_repository_keeps_it_open(cx: &mut TestAppContext) {
        let (_settings_dir, settings_path, state) = state_saving_settings(cx);
        let repo = repo_without_hooks();
        open_folder(cx, &state, repo.path());
        let entry = state.read_with(cx, |state, _| state.recent_repositories()[0].clone());

        state.update(cx, |state, cx| state.forget_repository(&entry, cx));

        assert!(listed(cx, &state).is_empty());
        assert!(Settings::load(&settings_path).repositories.is_empty());
        assert_eq!(open_root(cx, &state), repo.path().canonicalize().unwrap());
    }

    #[gpui::test]
    async fn switching_to_a_branch_another_worktree_has_opens_that_worktree(
        cx: &mut TestAppContext,
    ) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        let state = open_trusted(cx, &main);
        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();
        let feature = state.read_with(cx, |state, _| {
            let branches = &state.branches().unwrap().local;
            branches
                .iter()
                .find(|b| b.name == "feature")
                .unwrap()
                .clone()
        });

        state.update(cx, |state, cx| state.switch_branch(feature, cx));
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), linked.canonicalize().unwrap());
        assert_eq!(current_branch(&main), "main");
        assert!(state.read_with(cx, |state, _| state.error().is_none()));
    }

    #[gpui::test]
    async fn switching_to_a_missing_worktree_is_refused(cx: &mut TestAppContext) {
        let (_parent, main, linked) = repo_with_linked_worktree();
        fs::remove_dir_all(&linked).unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main.clone(), cx));
        cx.executor().run_until_parked();
        let missing = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| state.switch_worktree(&missing, cx));
        cx.executor().run_until_parked();

        assert_eq!(open_root(cx, &state), main.canonicalize().unwrap());
        assert!(state.read_with(cx, |state, _| state.error().is_none()));
    }

    #[gpui::test]
    async fn an_undo_from_one_worktree_is_not_offered_in_another(cx: &mut TestAppContext) {
        // Both worktrees sit on the same commit, so HEAD alone can't tell them apart.
        let (_parent, main, _linked) = repo_with_linked_worktree();
        fs::write(main.join("a.txt"), "my work\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(main, cx));
        cx.executor().run_until_parked();
        let changed = state.read_with(cx, |state, _| state.status().unwrap().files.clone());
        state.update(cx, |state, cx| state.discard(changed, cx));
        cx.executor().run_until_parked();
        assert!(state.read_with(cx, |state, _| state.last_discard().is_some()));
        let linked = worktree_at(cx, &state, 1);

        state.update(cx, |state, cx| state.switch_worktree(&linked, cx));
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.last_discard().is_none()));
    }

    #[gpui::test]
    async fn a_failed_clone_reports_why_and_opens_nothing(cx: &mut TestAppContext) {
        let parent = TempDir::new().unwrap();
        let missing = parent.path().join("no-such-repo");
        let state = cx.new(|_| RepoState::default());

        state.update(cx, |state, cx| {
            state.clone_repository(
                missing.to_str().unwrap().to_owned(),
                parent.path().join("copy"),
                cx,
            )
        });
        settle(cx, &state);

        state.read_with(cx, |state, _| {
            assert!(state.error().is_some());
            assert!(state.repo().is_none());
            assert!(state.operation().is_none());
        });
        assert!(!parent.path().join("copy").exists());
    }

    /// Two clones that have each committed since the remote last moved.
    fn diverged_clones() -> (TempDir, TempDir, TempDir) {
        let (remote, a, b) = remote_and_two_clones();
        fs::write(b.path().join("from-b.txt"), "b\n").unwrap();
        git(b.path(), &["add", "."]);
        git(b.path(), &["commit", "-q", "-m", "b"]);
        git(b.path(), &["push", "-q", "origin", "main"]);
        fs::write(a.path().join("from-a.txt"), "a\n").unwrap();
        git(a.path(), &["add", "."]);
        git(a.path(), &["commit", "-q", "-m", "a"]);
        (remote, a, b)
    }

    fn remote_has_file(remote: &Path, name: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(remote)
            .args(["cat-file", "-e", &format!("main:{name}")])
            .status()
            .unwrap()
            .success()
    }

    #[gpui::test]
    async fn pull_then_push_fixes_a_rejected_push(cx: &mut TestAppContext) {
        let (remote, a, _b) = diverged_clones();
        let state = open_trusted(cx, a.path());

        state.update(cx, |state, cx| state.sync(SyncKind::Push, cx));
        settle(cx, &state);
        assert!(
            state.read_with(cx, |s, _| s.error().is_some()),
            "push should be rejected"
        );

        state.update(cx, |state, cx| state.pull_then_push(cx));
        settle(cx, &state);

        state.read_with(cx, |s, _| assert!(s.error().is_none(), "{:?}", s.error()));
        assert!(remote_has_file(remote.path(), "from-a.txt"));
        assert!(remote_has_file(remote.path(), "from-b.txt"));
    }

    #[gpui::test]
    async fn a_conflicting_pull_then_push_stops_for_resolving_and_does_not_push(
        cx: &mut TestAppContext,
    ) {
        let (remote, a, b) = remote_and_two_clones();
        fs::write(b.path().join("a.txt"), "B\n").unwrap();
        git(b.path(), &["commit", "-qam", "b edit"]);
        git(b.path(), &["push", "-q", "origin", "main"]);
        fs::write(a.path().join("a.txt"), "A\n").unwrap();
        git(a.path(), &["commit", "-qam", "a edit"]);
        let state = open_trusted(cx, a.path());

        state.update(cx, |state, cx| state.pull_then_push(cx));
        settle(cx, &state);

        state.read_with(cx, |s, _| {
            assert!(s.error().is_none(), "{:?}", s.error());
            assert_eq!(s.conflicted_paths(), ["a.txt"]);
            assert!(s.in_progress().is_some());
        });
        let remote_main = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(remote.path())
                .args(["show", "main:a.txt"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(remote_main, "B\n", "nothing may be pushed after a conflict");
    }

    #[gpui::test]
    async fn retry_repeats_the_sync_that_failed(cx: &mut TestAppContext) {
        let (_remote, a, _b) = diverged_clones();
        let state = open_trusted(cx, a.path());
        state.update(cx, |state, cx| state.sync(SyncKind::Pull, cx));
        settle(cx, &state);
        assert!(state.read_with(cx, |s, _| s.error().is_some()));

        state.update(cx, |state, cx| state.retry_sync(cx));
        assert_eq!(
            state.read_with(cx, |s, _| s.operation().map(|o| o.kind)),
            Some(SyncKind::Pull)
        );
        assert!(state.read_with(cx, |s, _| s.error().is_none()));
        settle(cx, &state);
    }

    #[gpui::test]
    async fn stashing_sets_work_aside_and_the_list_shows_it(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
        let state = open_trusted(cx, dir.path());

        state.update(cx, |state, cx| state.stash_changes("my note".into(), cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.error().is_none(), "{:?}", s.error());
            let stashes = s.stashes().expect("loaded");
            assert_eq!(stashes.len(), 1);
            assert_eq!(stashes[0].message, "my note");
            assert!(s.status().unwrap().files.is_empty());
        });
    }

    #[gpui::test]
    async fn popping_a_stash_returns_the_work_and_empties_the_list(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
        let state = open_trusted(cx, dir.path());
        state.update(cx, |state, cx| state.stash_changes("n".into(), cx));
        cx.executor().run_until_parked();
        let stash = state.read_with(cx, |s, _| s.stashes().unwrap()[0].clone());

        state.update(cx, |state, cx| state.pop_stash(stash, cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.error().is_none(), "{:?}", s.error());
            assert!(s.stashes().unwrap().is_empty());
            // The edited a.txt and the untracked b.txt both came back.
            assert_eq!(s.status().unwrap().files.len(), 2);
        });
    }

    #[gpui::test]
    async fn applying_a_stash_needs_trust_but_stashing_and_dropping_do_not(
        cx: &mut TestAppContext,
    ) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        assert!(!state.read_with(cx, |s, _| s.is_trusted()));

        state.update(cx, |state, cx| state.stash_changes("n".into(), cx));
        cx.executor().run_until_parked();
        let stash = state.read_with(cx, |s, _| s.stashes().unwrap()[0].clone());

        state.update(cx, |state, cx| state.apply_stash(stash.clone(), cx));
        cx.executor().run_until_parked();
        state.read_with(cx, |s, _| {
            assert!(s.error().is_some_and(|e| e.contains("Trust")));
            assert!(
                s.status().unwrap().files.is_empty(),
                "nothing may be applied"
            );
        });

        state.update(cx, |state, cx| state.dismiss_error(cx));
        state.update(cx, |state, cx| state.drop_stash(stash, cx));
        cx.executor().run_until_parked();
        state.read_with(cx, |s, _| assert!(s.stashes().unwrap().is_empty()));
    }

    #[gpui::test]
    async fn auto_fetch_brings_in_news_without_an_error_or_a_retry_slot(cx: &mut TestAppContext) {
        let (_remote, a, b) = remote_and_two_clones();
        fs::write(b.path().join("news.txt"), "n\n").unwrap();
        git(b.path(), &["add", "."]);
        git(b.path(), &["commit", "-q", "-m", "news"]);
        git(b.path(), &["push", "-q", "origin", "main"]);
        let state = open_trusted(cx, a.path());

        state.update(cx, |state, cx| state.auto_fetch(cx));
        settle(cx, &state);

        state.read_with(cx, |s, _| {
            assert_eq!(s.status().unwrap().branch.behind, 1);
            assert!(s.error().is_none());
            assert!(!s.can_retry(), "a silent fetch is not something to retry");
        });
    }

    #[gpui::test]
    async fn auto_fetch_stays_quiet_when_it_fails_and_keeps_an_earlier_error(
        cx: &mut TestAppContext,
    ) {
        let (remote, a, _b) = remote_and_two_clones();
        let state = open_trusted(cx, a.path());
        state.update(cx, |state, cx| state.report("earlier problem".into(), cx));
        // Break the remote so the fetch fails.
        fs::remove_dir_all(remote.path().join("objects")).unwrap();
        fs::remove_dir_all(remote.path().join("refs")).unwrap();

        state.update(cx, |state, cx| state.auto_fetch(cx));
        settle(cx, &state);

        state.read_with(cx, |s, _| {
            assert_eq!(s.error().map(|e| e.as_ref()), Some("earlier problem"))
        });
    }

    #[gpui::test]
    async fn a_successful_auto_fetch_does_not_wipe_an_error_the_user_has_not_seen(
        cx: &mut TestAppContext,
    ) {
        let (_remote, a, _b) = remote_and_two_clones();
        let state = open_trusted(cx, a.path());
        state.update(cx, |state, cx| {
            state.report("commit failed: lint".into(), cx)
        });

        state.update(cx, |state, cx| state.auto_fetch(cx));
        settle(cx, &state);

        state.read_with(cx, |s, _| {
            assert_eq!(s.error().map(|e| e.as_ref()), Some("commit failed: lint"))
        });
    }

    #[gpui::test]
    async fn auto_fetch_does_nothing_in_an_untrusted_repository(cx: &mut TestAppContext) {
        let (_remote, a, _b) = remote_and_two_clones();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(a.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.auto_fetch(cx));

        state.read_with(cx, |s, _| {
            assert!(s.operation().is_none());
            assert!(
                s.error().is_none(),
                "no trust prompt for a background fetch"
            );
        });
    }

    #[gpui::test]
    async fn auto_fetch_does_nothing_without_a_remote(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = open_trusted(cx, dir.path());
        state.update(cx, |state, cx| state.auto_fetch(cx));
        state.read_with(cx, |s, _| assert!(s.operation().is_none()));
    }

    #[gpui::test]
    async fn pushing_sends_the_commit_and_clears_the_ahead_count(cx: &mut TestAppContext) {
        let (remote, a, _b) = remote_and_two_clones();
        fs::write(a.path().join("mine.txt"), "m\n").unwrap();
        git(a.path(), &["add", "."]);
        git(a.path(), &["commit", "-q", "-m", "mine"]);
        let state = open_trusted(cx, a.path());
        assert_eq!(
            state.read_with(cx, |s, _| s.status().unwrap().branch.ahead),
            1
        );

        state.update(cx, |state, cx| state.sync(SyncKind::Push, cx));
        settle(cx, &state);

        assert_eq!(
            state.read_with(cx, |s, _| s.status().unwrap().branch.ahead),
            0
        );
        // The remote now has everything the clone has.
        assert_eq!(commit_count(remote.path()), commit_count(a.path()));
    }

    #[gpui::test]
    async fn a_pull_that_cannot_fast_forward_reports_why_and_changes_nothing(
        cx: &mut TestAppContext,
    ) {
        let (_remote, a, b) = remote_and_two_clones();
        for (dir, name) in [(b.path(), "theirs.txt"), (a.path(), "ours.txt")] {
            fs::write(dir.join(name), "x\n").unwrap();
            git(dir, &["add", "."]);
            git(dir, &["commit", "-q", "-m", name]);
        }
        git(b.path(), &["push", "-q", "origin", "main"]);
        let state = open_trusted(cx, a.path());

        state.update(cx, |state, cx| state.sync(SyncKind::Pull, cx));
        settle(cx, &state);

        state.read_with(cx, |state, _| {
            assert!(
                state
                    .error()
                    .unwrap()
                    .to_lowercase()
                    .contains("fast-forward")
            );
            assert_eq!(
                state.status().unwrap().branch.ahead,
                1,
                "our commit is still there"
            );
        });
        assert!(!a.path().join("theirs.txt").exists());
    }

    #[gpui::test]
    async fn an_untrusted_repository_will_not_talk_to_the_remote(cx: &mut TestAppContext) {
        let (_remote, a, _b) = remote_and_two_clones();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(a.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.sync(SyncKind::Fetch, cx));

        state.read_with(cx, |state, _| {
            assert!(state.operation().is_none(), "nothing should have started");
            assert!(state.error().unwrap().contains("Trust this repository"));
        });
    }

    #[gpui::test]
    async fn a_second_sync_while_one_runs_is_ignored(cx: &mut TestAppContext) {
        let (_remote, a, _b) = remote_and_two_clones();
        let state = open_trusted(cx, a.path());

        state.update(cx, |state, cx| {
            state.sync(SyncKind::Fetch, cx);
            state.sync(SyncKind::Pull, cx);
            assert_eq!(state.operation().unwrap().kind, SyncKind::Fetch);
        });
        settle(cx, &state);
    }

    #[gpui::test]
    async fn publishing_a_new_branch_makes_the_remote_one_its_upstream(cx: &mut TestAppContext) {
        let (_remote, a, _b) = remote_and_two_clones();
        git(a.path(), &["switch", "-q", "-c", "feat/new"]);
        let state = open_trusted(cx, a.path());
        assert_eq!(
            state.read_with(cx, |s, _| s.status().unwrap().branch.upstream.clone()),
            None
        );

        state.update(cx, |state, cx| state.sync(SyncKind::Publish, cx));
        settle(cx, &state);

        assert_eq!(
            state.read_with(cx, |s, _| s.status().unwrap().branch.upstream.clone()),
            Some("origin/feat/new".to_owned())
        );
    }

    #[gpui::test]
    async fn discarding_removes_the_changes_and_can_be_undone(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "my work\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        let file = state.read_with(cx, |s, _| {
            s.status()
                .unwrap()
                .files
                .iter()
                .find(|f| f.path == "a.txt")
                .unwrap()
                .clone()
        });

        state.update(cx, |state, cx| state.discard(vec![file], cx));
        cx.executor().run_until_parked();

        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "a\n");
        state.read_with(cx, |state, _| {
            assert!(
                !state
                    .status()
                    .unwrap()
                    .files
                    .iter()
                    .any(|f| f.path == "a.txt")
            );
            assert_eq!(state.last_discard(), Some("Discarded changes to a.txt"));
        });

        state.update(cx, |state, cx| state.undo_discard(cx));
        cx.executor().run_until_parked();

        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "my work\n"
        );
        state.read_with(cx, |state, _| assert_eq!(state.last_discard(), None));
    }

    #[gpui::test]
    async fn a_new_discard_replaces_the_previous_undo(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        let pick = |cx: &mut TestAppContext, name: &str| {
            state.read_with(cx, |s, _| {
                s.status()
                    .unwrap()
                    .files
                    .iter()
                    .find(|f| f.path == name)
                    .unwrap()
                    .clone()
            })
        };

        let a = pick(cx, "a.txt");
        state.update(cx, |state, cx| state.discard(vec![a], cx));
        cx.executor().run_until_parked();
        let b = pick(cx, "b.txt");
        state.update(cx, |state, cx| state.discard(vec![b], cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.last_discard(), Some("Discarded changes to b.txt"));
        });
    }

    fn last_subject(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["log", "-1", "--format=%s"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[gpui::test]
    async fn an_untrusted_repository_refuses_to_amend(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.amend("new".into(), String::new(), cx));
        cx.executor().run_until_parked();

        assert_eq!(last_subject(dir.path()), "base");
        state.read_with(cx, |state, _| {
            assert!(
                state
                    .error()
                    .unwrap()
                    .contains("Trust this repository to amend")
            );
        });
    }

    #[gpui::test]
    async fn amending_rewrites_the_last_commit_and_announces_it(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        let commits_before = commit_count(dir.path());
        let announced = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = announced.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_, event: &RepoEvent, _| {
                if *event == RepoEvent::Committed {
                    seen.set(true);
                }
            })
        });

        state.update(cx, |state, cx| {
            state.amend("Reworded".into(), String::new(), cx)
        });
        cx.executor().run_until_parked();

        assert_eq!(last_subject(dir.path()), "Reworded");
        assert_eq!(commit_count(dir.path()), commits_before, "no new commit");
        assert!(announced.get(), "the message boxes should be told to clear");
    }

    fn local_branch_names(cx: &mut TestAppContext, state: &gpui::Entity<RepoState>) -> Vec<String> {
        state.read_with(cx, |state, _| {
            state
                .branches()
                .unwrap()
                .local
                .iter()
                .map(|b| b.name.clone())
                .collect()
        })
    }

    #[gpui::test]
    async fn renaming_a_branch_reloads_the_list(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["branch", "old"]);
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| {
            state.rename_branch("old".into(), "new".into(), cx)
        });
        cx.executor().run_until_parked();

        let names = local_branch_names(cx, &state);
        assert!(names.contains(&"new".to_owned()) && !names.contains(&"old".to_owned()));
    }

    #[gpui::test]
    async fn deleting_a_branch_removes_it_from_the_list(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["branch", "gone"]);
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        state.update(cx, |state, cx| state.load_branches(cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.delete_branch("gone".into(), cx));
        cx.executor().run_until_parked();

        assert!(!local_branch_names(cx, &state).contains(&"gone".to_owned()));
    }

    #[gpui::test]
    async fn undoing_the_last_commit_announces_its_message(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        git(dir.path(), &["add", "b.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "Add b", "-m", "Why b."]);
        // Untrusted on purpose: undo writes a ref with hooks off, like stash.
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        let announced = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = announced.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_, event: &RepoEvent, _| {
                if let RepoEvent::CommitUndone {
                    summary,
                    description,
                } = event
                {
                    *seen.borrow_mut() = Some((summary.clone(), description.clone()));
                }
            })
        });

        let head = state.read_with(cx, |state, _| state.status().unwrap().branch.oid.clone());
        state.update(cx, |state, cx| state.undo_last_commit(head.unwrap(), cx));
        cx.executor().run_until_parked();

        assert_eq!(last_subject(dir.path()), "base");
        assert_eq!(
            *announced.borrow(),
            Some(("Add b".to_owned(), "Why b.".to_owned()))
        );
        let staged = state.read_with(cx, |state, _| {
            state.status().unwrap().files.iter().all(|f| f.is_staged())
        });
        assert!(staged, "the undone commit's changes stay included");
    }

    #[gpui::test]
    async fn the_undo_lapses_when_head_moves(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "my work\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        let file = state.read_with(cx, |s, _| {
            s.status()
                .unwrap()
                .files
                .iter()
                .find(|f| f.path == "a.txt")
                .unwrap()
                .clone()
        });
        state.update(cx, |state, cx| state.discard(vec![file], cx));
        cx.executor().run_until_parked();
        assert!(state.read_with(cx, |state, _| state.last_discard().is_some()));

        // A commit made elsewhere (a terminal) moves HEAD; the next refresh notices.
        git(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "elsewhere"],
        );
        state.update(cx, |state, cx| state.refresh(cx));
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.last_discard().is_none()));
    }

    #[gpui::test]
    async fn the_undo_survives_an_ordinary_refresh(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("a.txt"), "my work\n").unwrap();
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        let file = state.read_with(cx, |s, _| {
            s.status()
                .unwrap()
                .files
                .iter()
                .find(|f| f.path == "a.txt")
                .unwrap()
                .clone()
        });
        state.update(cx, |state, cx| state.discard(vec![file], cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.refresh(cx));
        cx.executor().run_until_parked();

        assert!(state.read_with(cx, |state, _| state.last_discard().is_some()));
    }

    fn head_commit(
        cx: &mut TestAppContext,
        state: &gpui::Entity<RepoState>,
    ) -> git_core::CommitInfo {
        let repo = state.read_with(cx, |s, _| s.repo().unwrap().clone());
        repo.log(0, 1).unwrap().remove(0)
    }

    #[gpui::test]
    async fn an_untrusted_repository_will_not_revert_cherry_pick_or_merge(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["branch", "other"]);
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        let commit = head_commit(cx, &state);
        let before = commit_count(dir.path());

        state.update(cx, |state, cx| state.revert_commit(commit.clone(), cx));
        state.read_with(cx, |s, _| {
            assert!(s.error().unwrap().contains("revert a commit"))
        });
        state.update(cx, |state, cx| state.cherry_pick_commit(commit, cx));
        state.read_with(cx, |s, _| {
            assert!(s.error().unwrap().contains("cherry-pick a commit"))
        });
        state.update(cx, |state, cx| state.merge_branch("other".into(), cx));
        state.read_with(cx, |s, _| {
            assert!(s.error().unwrap().contains("merge a branch"))
        });
        cx.executor().run_until_parked();

        assert_eq!(commit_count(dir.path()), before);
    }

    #[gpui::test]
    async fn a_trusted_repository_reverts_a_commit(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        fs::write(dir.path().join("extra.txt"), "x\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "add extra"]);
        let state = open_trusted(cx, dir.path());
        let target = head_commit(cx, &state);

        state.update(cx, |state, cx| state.revert_commit(target, cx));
        cx.executor().run_until_parked();

        assert!(!dir.path().join("extra.txt").exists());
        assert!(last_subject(dir.path()).starts_with("Revert"));
        // The status the UI shows follows (HEAD moved), which is what refreshes History.
        state.read_with(cx, |s, _| assert!(s.error().is_none()));
    }

    #[gpui::test]
    async fn a_conflicting_merge_stays_in_progress_without_an_error_banner(
        cx: &mut TestAppContext,
    ) {
        let dir = repo_without_hooks();
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("a.txt"), "feature side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "feature edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "main side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "main edit"]);
        let state = open_trusted(cx, dir.path());

        state.update(cx, |state, cx| state.merge_branch("feature".into(), cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.error().is_none(), "{:?}", s.error());
            assert_eq!(s.conflicted_paths(), ["a.txt"]);
            let progress = s.in_progress().expect("the merge is waiting");
            assert_eq!(progress.merging.as_deref(), Some("feature"));
        });
    }

    #[gpui::test]
    async fn resolving_and_continuing_finishes_the_merge(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("a.txt"), "feature side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "feature edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "main side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "main edit"]);
        let state = open_trusted(cx, dir.path());
        state.update(cx, |state, cx| state.merge_branch("feature".into(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| {
            state.resolve_with_text("a.txt".into(), "resolved\n".into(), cx)
        });
        cx.executor().run_until_parked();
        state.read_with(cx, |s, _| {
            assert!(s.conflicted_paths().is_empty());
            assert!(s.in_progress().is_some(), "still waiting to be continued");
        });

        state.update(cx, |state, cx| state.continue_operation(cx));
        cx.executor().run_until_parked();
        state.read_with(cx, |s, _| {
            assert!(s.error().is_none(), "{:?}", s.error());
            assert!(s.in_progress().is_none());
        });
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "resolved\n"
        );
    }

    #[gpui::test]
    async fn aborting_puts_everything_back(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("a.txt"), "feature side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "feature edit"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "main side\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "main edit"]);
        let state = open_trusted(cx, dir.path());
        state.update(cx, |state, cx| state.merge_branch("feature".into(), cx));
        cx.executor().run_until_parked();

        state.update(cx, |state, cx| state.abort_operation(cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.in_progress().is_none());
            assert!(s.conflicted_paths().is_empty());
        });
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "main side\n"
        );
    }

    #[gpui::test]
    async fn continuing_needs_trust(cx: &mut TestAppContext) {
        let dir = repo_without_hooks();
        git(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("a.txt"), "f\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "f"]);
        git(dir.path(), &["switch", "-q", "main"]);
        fs::write(dir.path().join("a.txt"), "m\n").unwrap();
        git(dir.path(), &["commit", "-q", "-a", "-m", "m"]);
        assert!(
            !crate::testutil::git_ok(dir.path(), &["merge", "feature"]),
            "the merge should conflict"
        );
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.executor().run_until_parked();
        assert!(!state.read_with(cx, |s, _| s.is_trusted()));

        state.update(cx, |state, cx| {
            state.resolve_with_text("a.txt".into(), "x\n".into(), cx)
        });
        cx.executor().run_until_parked();
        state.update(cx, |state, cx| state.continue_operation(cx));
        cx.executor().run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.error().is_some_and(|e| e.contains("Trust")));
            assert!(s.in_progress().is_some(), "nothing was committed");
        });
    }
}
