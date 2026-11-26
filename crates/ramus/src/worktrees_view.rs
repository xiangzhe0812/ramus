use std::path::PathBuf;

use git_core::{Head, RemovalCheck, Worktree, worktree_folder_name};
use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    diff_view::message,
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    ui::{abbreviate_home, panel},
};

/// The Worktrees tab: the repository's other checkouts, opening one, making a new one on
/// a new branch, and removing one.
pub struct WorktreesView {
    repo_state: Entity<RepoState>,
    branch: Entity<InputState>,
    /// The worktree being looked at, by folder.
    selected: Option<PathBuf>,
    /// What removing the selected worktree would lose; `None` while it is being worked out.
    check: Option<RemovalCheck>,
    /// The worktree a first click on "Remove" asked about; a second click confirms.
    confirm_remove: Option<PathBuf>,
    /// Also delete the branch when removing.
    delete_branch: bool,
    /// The list was asked for and hasn't arrived, so don't ask again.
    requested: bool,
    check_generation: u64,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// The name a worktree goes by: its branch, or the commit it is detached at.
fn title(worktree: &Worktree) -> String {
    match &worktree.head {
        Head::Branch(name) => name.clone(),
        Head::Detached(sha) => format!("detached at {sha}"),
    }
}

/// A line saying what removing would throw away, or `None` if nothing is at risk.
fn loss_warning(check: &RemovalCheck) -> Option<String> {
    let files = |n: usize| format!("{n} uncommitted file{}", if n == 1 { "" } else { "s" });
    let commits = |n: usize| format!("{n} commit{}", if n == 1 { "" } else { "s" });
    match (check.uncommitted_files, check.unique_commits) {
        (0, 0) => None,
        (f, 0) => Some(format!("Removing discards {}.", files(f))),
        (0, c) => Some(format!(
            "{} on this branch exist nowhere else; they stay reachable only through the reflog.",
            commits(c)
        )),
        (f, c) => Some(format!(
            "Removing discards {}, and {} on this branch exist nowhere else.",
            files(f),
            commits(c)
        )),
    }
}

impl WorktreesView {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let branch = cx.new(|cx| InputState::new(window, cx).placeholder("New branch name"));
        let _subscriptions = vec![
            cx.observe(&repo_state, |this, _, cx| this.sync(cx)),
            cx.subscribe_in(
                &branch,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.create_now(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
        ];
        let mut this = Self {
            repo_state,
            branch,
            selected: None,
            check: None,
            confirm_remove: None,
            delete_branch: false,
            requested: false,
            check_generation: 0,
            tasks: Vec::new(),
            _subscriptions,
        };
        this.sync(cx);
        this
    }

    /// Asks for the list when it isn't there, and keeps the selection pointing at a
    /// worktree that still exists.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.repo_state.read(cx);
        if state.repo().is_none() {
            self.requested = false;
            return;
        }
        let Some(worktrees) = state.worktrees() else {
            if !self.requested {
                self.requested = true;
                self.repo_state
                    .update(cx, |state, cx| state.load_worktrees(cx));
            }
            return;
        };
        self.requested = false;
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|path| worktrees.iter().any(|w| &w.path == path));
        if !still_there {
            // Start on the worktree that is open, else the first.
            let open = state.repo().map(|repo| repo.root().to_owned());
            let pick = worktrees
                .iter()
                .find(|w| open.as_deref() == Some(w.path.as_path()))
                .or_else(|| worktrees.first())
                .map(|w| w.path.clone());
            self.select(pick, cx);
        }
        cx.notify();
    }

    fn worktrees(&self, cx: &gpui::App) -> Vec<Worktree> {
        self.repo_state
            .read(cx)
            .worktrees()
            .map(<[Worktree]>::to_vec)
            .unwrap_or_default()
    }

    fn selected_worktree(&self, cx: &gpui::App) -> Option<Worktree> {
        let path = self.selected.as_ref()?;
        self.worktrees(cx).into_iter().find(|w| &w.path == path)
    }

    fn select(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.selected = path;
        self.confirm_remove = None;
        self.delete_branch = false;
        self.check = None;
        self.check_generation += 1;
        let (Some(repo), Some(worktree)) = (
            self.repo_state.read(cx).repo().cloned(),
            self.selected_worktree(cx),
        ) else {
            cx.notify();
            return;
        };
        if !worktree.is_main {
            let generation = self.check_generation;
            self.tasks.push(cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { repo.removal_check(&worktree) })
                    .await;
                this.update(cx, |this, cx| {
                    if this.check_generation != generation {
                        return;
                    }
                    match result {
                        Ok(check) => this.check = Some(check),
                        Err(error) => this
                            .repo_state
                            .update(cx, |state, cx| state.report(error.to_string(), cx)),
                    }
                    cx.notify();
                })
                .ok();
            }));
        }
        cx.notify();
    }

    fn create_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.branch.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.branch
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.repo_state
            .update(cx, |state, cx| state.add_worktree(name, cx));
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.selected_worktree(cx) {
            self.repo_state
                .update(cx, |state, cx| state.switch_worktree(&worktree, cx));
        }
    }

    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.selected_worktree(cx) else {
            return;
        };
        // Wait for the check: confirming before it arrives would be confirming blind.
        let Some(check) = self.check else {
            return;
        };
        if self.confirm_remove.as_ref() == Some(&worktree.path) {
            self.confirm_remove = None;
            let force = check.uncommitted_files > 0;
            let delete_branch = self.delete_branch;
            self.repo_state.update(cx, |state, cx| {
                state.remove_worktree(worktree, force, delete_branch, cx)
            });
        } else {
            self.confirm_remove = Some(worktree.path);
        }
        cx.notify();
    }

    fn detail(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(worktree) = self.selected_worktree(cx) else {
            return message(theme, "Select a worktree");
        };
        let button = |id: &'static str| {
            div()
                .id(id)
                .h(px(theme.metrics.control_height))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|b| b.bg(theme.surface_selected))
        };
        let state = self.repo_state.read(cx);
        let busy = state.is_busy();
        let is_open = state
            .repo()
            .is_some_and(|repo| repo.root() == worktree.path.as_path());
        let confirming = self.confirm_remove.as_ref() == Some(&worktree.path);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let location = abbreviate_home(&worktree.path, home.as_deref());
        let branch_name = match &worktree.head {
            Head::Branch(name) => Some(name.clone()),
            Head::Detached(_) => None,
        };

        let header = div()
            .flex_none()
            .px(px(18.))
            .py(px(14.))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(theme.metrics.text_lg))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(SharedString::from(title(&worktree))),
                    )
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(theme.metrics.text_sm))
                            .text_color(theme.text_muted)
                            .truncate()
                            .child(SharedString::from(location)),
                    ),
            )
            .child(
                button("open-worktree")
                    .bg(theme.accent)
                    .border_color(theme.accent)
                    .text_color(theme.accent_text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .when(is_open || worktree.missing, |b| b.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| this.open_selected(cx)))
                    .child(if is_open { "Open here" } else { "Open" }),
            )
            .when(!worktree.is_main, |header| {
                header.child(
                    button("remove-worktree")
                        .border_color(theme.error_border)
                        .text_color(theme.error_text)
                        .when(confirming, |b| b.bg(theme.error_background))
                        .when(self.check.is_none() || busy, |b| b.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.remove_selected(cx)))
                        .child(if confirming {
                            "Click again to remove"
                        } else {
                            "Remove"
                        }),
                )
            });

        let note = |text: String, color| {
            div()
                .text_size(px(theme.metrics.text_base))
                .text_color(color)
                .child(SharedString::from(text))
        };
        let mut body = div()
            .flex_1()
            .min_h_0()
            .p(px(18.))
            .flex()
            .flex_col()
            .gap(px(10.));
        if worktree.is_main {
            body = body.child(note(
                "The main worktree holds the repository's .git folder and can't be removed.".into(),
                theme.text_muted,
            ));
        }
        if worktree.missing {
            body = body.child(note(
                "Its folder is gone. Removing it clears git's record of it.".into(),
                theme.error_text,
            ));
        }
        if worktree.locked {
            body = body.child(note(
                "Locked: git won't remove it until it is unlocked.".into(),
                theme.text_muted,
            ));
        }
        if !worktree.is_main {
            match self.check {
                None => {
                    body = body.child(note(
                        "Checking what removing would lose…".into(),
                        theme.text_muted,
                    ))
                }
                Some(check) => {
                    if let Some(warning) = loss_warning(&check) {
                        body = body.child(note(warning, theme.error_text));
                    }
                }
            }
        }
        if let (false, Some(branch)) = (worktree.is_main, branch_name) {
            let on = self.delete_branch;
            body = body.child(
                div()
                    .id("delete-branch")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.delete_branch = !this.delete_branch;
                        this.confirm_remove = None;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded(px(theme.metrics.radius_sm))
                            .border_1()
                            .border_color(theme.border)
                            .when(on, |b| b.bg(theme.accent)),
                    )
                    .child(SharedString::from(format!("Also delete branch {branch}"))),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl Render for WorktreesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let state = self.repo_state.read(cx);
        let has_repo = state.repo().is_some();
        let loaded = state.worktrees().is_some();
        let busy = state.is_busy();
        let open = state.repo().map(|repo| repo.root().to_owned());
        let worktrees = self.worktrees(cx);
        let home = std::env::var_os("HOME").map(PathBuf::from);

        let list: AnyElement = if !has_repo {
            message(&theme, "No repository open")
        } else if !loaded {
            message(&theme, "Loading worktrees…")
        } else {
            div()
                .id("worktree-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .py(px(6.))
                .children(worktrees.iter().enumerate().map(|(ix, worktree)| {
                    let path = worktree.path.clone();
                    let selected = self.selected.as_ref() == Some(&path);
                    let is_open = open.as_deref() == Some(worktree.path.as_path());
                    let badge = |text: &'static str| {
                        div()
                            .px(px(7.))
                            .rounded(px(theme.metrics.radius_md))
                            .bg(theme.pill_active)
                            .text_size(px(theme.metrics.text_xs))
                            .text_color(theme.text_muted)
                            .child(text)
                    };
                    div()
                        .id(("worktree", ix))
                        .h(px(44.))
                        .px(px(10.))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap(px(3.))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(theme.surface_selected))
                        .hover(|row| row.bg(theme.surface_selected))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select(Some(path.clone()), cx)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(SharedString::from(title(worktree))),
                                )
                                .when(worktree.is_main, |row| row.child(badge("main")))
                                .when(is_open, |row| row.child(badge("open")))
                                .when(worktree.locked, |row| row.child(badge("locked")))
                                .when(worktree.missing, |row| row.child(badge("missing"))),
                        )
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .text_size(px(theme.metrics.text_xs))
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(SharedString::from(abbreviate_home(
                                    &worktree.path,
                                    home.as_deref(),
                                ))),
                        )
                }))
                .into_any_element()
        };

        let detail = self.detail(&theme, cx);
        let typed = self.branch.read(cx).value().trim().to_owned();
        let hint = if typed.is_empty() {
            "Made next to the main folder, off the default branch.".to_owned()
        } else {
            format!("Folder: ../{}", worktree_folder_name(&typed))
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(12.))
            .px(px(12.))
            .pb(px(12.))
            .child(
                panel(&theme)
                    .w(px(360.))
                    .flex_none()
                    .child(
                        div()
                            .h(px(40.))
                            .flex_none()
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Worktrees"),
                            )
                            .child(
                                div()
                                    .px(px(9.))
                                    .py(px(2.))
                                    .rounded(px(theme.metrics.radius_md))
                                    .bg(theme.pill_active)
                                    .text_size(px(theme.metrics.text_sm))
                                    .text_color(theme.text_muted)
                                    .child(worktrees.len().to_string()),
                            ),
                    )
                    .child(list)
                    .child(
                        div()
                            .flex_none()
                            .p(px(12.))
                            .border_t_1()
                            .border_color(theme.border)
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(Input::new(&self.branch).aria_label("New worktree branch"))
                            .child(
                                div()
                                    .text_size(px(theme.metrics.text_sm))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(hint)),
                            )
                            .child(
                                div()
                                    .id("create-worktree")
                                    .h(px(theme.metrics.control_height))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(theme.metrics.radius_md))
                                    .bg(theme.accent)
                                    .text_color(theme.accent_text)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .when(!has_repo || busy || typed.is_empty(), |b| b.opacity(0.5))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create_now(window, cx)
                                    }))
                                    .child("Create worktree"),
                            ),
                    ),
            )
            .child(panel(&theme).flex_1().min_w_0().child(detail))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gpui::{TestAppContext, VisualTestContext};
    use tempfile::TempDir;

    use super::*;
    use crate::testutil::git;

    /// A trusted repo called `main` inside a scratch folder (worktrees are made beside it).
    fn open<'a>(
        cx: &'a mut TestAppContext,
        scratch: &TempDir,
    ) -> (
        Entity<RepoState>,
        Entity<WorktreesView>,
        &'a mut VisualTestContext,
        PathBuf,
    ) {
        let dir = scratch.path().join("main");
        fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "base"]);

        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.clone(), cx));
        cx.run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        let (view, cx) = cx.add_window_view({
            let state = state.clone();
            move |window, cx| WorktreesView::new(state, window, cx)
        });
        cx.run_until_parked();
        let dir = dir.canonicalize().unwrap();
        (state, view, cx, dir)
    }

    fn paths(state: &Entity<RepoState>, cx: &mut VisualTestContext) -> Vec<PathBuf> {
        state.read_with(cx, |s, _| {
            s.worktrees()
                .unwrap()
                .iter()
                .map(|w| w.path.clone())
                .collect()
        })
    }

    #[gpui::test]
    fn it_lists_the_main_worktree_and_selects_the_open_one(cx: &mut TestAppContext) {
        let scratch = TempDir::new().unwrap();
        let (state, view, cx, main) = open(cx, &scratch);

        assert_eq!(paths(&state, cx), vec![main.clone()]);
        view.read_with(cx, |v, _| assert_eq!(v.selected, Some(main)));
    }

    #[gpui::test]
    fn creating_a_worktree_adds_it_to_the_list(cx: &mut TestAppContext) {
        let scratch = TempDir::new().unwrap();
        let (state, view, cx, main) = open(cx, &scratch);

        view.update_in(cx, |v, window, cx| {
            v.branch
                .update(cx, |input, cx| input.set_value("feature/x", window, cx));
            v.create_now(window, cx);
        });
        cx.run_until_parked();

        let created = main.parent().unwrap().join("feature-x");
        assert!(created.is_dir());
        assert_eq!(paths(&state, cx), [main, created]);
        view.read_with(cx, |v, cx| {
            assert_eq!(v.branch.read(cx).value().as_ref(), "");
        });
    }

    #[gpui::test]
    fn an_untrusted_repository_will_not_create_a_worktree(cx: &mut TestAppContext) {
        let scratch = TempDir::new().unwrap();
        let (state, view, cx, main) = open(cx, &scratch);
        // A fresh state that never trusted the repo.
        let untrusted = cx.new(|_| RepoState::default());
        untrusted.update(cx, |s, cx| s.open(main.clone(), cx));
        cx.run_until_parked();

        untrusted.update(cx, |s, cx| s.add_worktree("nope".into(), cx));
        cx.run_until_parked();

        assert!(!main.parent().unwrap().join("nope").exists());
        assert!(untrusted.read_with(cx, |s, _| s.error().is_some()));
        drop((state, view));
    }

    #[gpui::test]
    fn opening_a_worktree_switches_the_window_to_it(cx: &mut TestAppContext) {
        let scratch = TempDir::new().unwrap();
        let (state, view, cx, main) = open(cx, &scratch);
        view.update_in(cx, |v, window, cx| {
            v.branch
                .update(cx, |input, cx| input.set_value("topic", window, cx));
            v.create_now(window, cx);
        });
        cx.run_until_parked();
        let topic = main.parent().unwrap().join("topic").canonicalize().unwrap();

        view.update(cx, |v, cx| {
            v.select(Some(topic.clone()), cx);
            v.open_selected(cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |s, _| {
            assert_eq!(s.repo().unwrap().root(), topic.as_path());
        });
    }

    #[gpui::test]
    fn removing_needs_a_second_click_and_the_open_worktree_falls_back_to_main(
        cx: &mut TestAppContext,
    ) {
        let scratch = TempDir::new().unwrap();
        let (state, view, cx, main) = open(cx, &scratch);
        view.update_in(cx, |v, window, cx| {
            v.branch
                .update(cx, |input, cx| input.set_value("topic", window, cx));
            v.create_now(window, cx);
        });
        cx.run_until_parked();
        let topic = main.parent().unwrap().join("topic").canonicalize().unwrap();
        view.update(cx, |v, cx| {
            v.select(Some(topic.clone()), cx);
            v.open_selected(cx);
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| v.select(Some(topic.clone()), cx));
        cx.run_until_parked();

        view.update(cx, |v, cx| v.remove_selected(cx));
        cx.run_until_parked();
        assert!(topic.is_dir(), "the first click only asks");

        view.update(cx, |v, cx| {
            v.delete_branch = true;
            v.confirm_remove = Some(topic.clone());
            v.remove_selected(cx)
        });
        cx.run_until_parked();

        assert!(!topic.exists());
        state.read_with(cx, |s, _| {
            assert_eq!(s.repo().unwrap().root(), main.as_path());
            assert_eq!(s.worktrees().unwrap().len(), 1);
            assert!(s.error().is_none(), "{:?}", s.error());
        });
        assert!(!crate::testutil::git_ok(
            &main,
            &["rev-parse", "--verify", "-q", "refs/heads/topic"]
        ));
    }

    #[test]
    fn the_warning_says_what_would_be_lost() {
        let none = RemovalCheck::default();
        assert_eq!(loss_warning(&none), None);
        let both = RemovalCheck {
            uncommitted_files: 2,
            unique_commits: 1,
        };
        let text = loss_warning(&both).unwrap();
        assert!(text.contains("2 uncommitted files") && text.contains("1 commit "));
    }
}
