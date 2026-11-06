use std::path::PathBuf;

use git_core::{Repo, RepoStatus};
use gpui::{Context, SharedString, Task};

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
}

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

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// Opens the repository containing `path`. On failure the current repo stays open.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load(None, path, cx);
    }

    /// Re-reads status for the open repository, if any.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.clone() {
            let root = repo.root().to_owned();
            self.load(Some(repo), root, cx);
        }
    }

    fn load(&mut self, known: Option<Repo>, path: PathBuf, cx: &mut Context<Self>) {
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
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }
}
