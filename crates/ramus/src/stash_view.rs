use git_core::{DiffOptions, FileDiffs, Stash, StashFile, relative_time};
use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    diff_rows::HunkControls,
    diff_view::{DiffContent, DiffInteraction, diff_body, message},
    history_view::now,
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
};

/// The Stashes tab: work set aside, and what is in each stash.
pub struct StashView {
    repo_state: Entity<RepoState>,
    note: Entity<InputState>,
    /// The stash being looked at, by commit (its position changes as others come and go).
    selected: Option<String>,
    files: Vec<StashFile>,
    selected_file: Option<usize>,
    diff: Option<DiffContent>,
    hide_whitespace: bool,
    /// The stash a first click on "Drop" asked about; a second click confirms.
    confirm_drop: Option<String>,
    /// The list was asked for and hasn't arrived, so don't ask again.
    requested: bool,
    detail_generation: u64,
    diff_generation: u64,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl StashView {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Message (optional)"));
        let _subscriptions = vec![
            cx.observe(&repo_state, |this, _, cx| this.sync(cx)),
            cx.subscribe_in(&note, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.stash_now(cx);
                }
            }),
        ];
        let mut this = Self {
            repo_state,
            note,
            selected: None,
            files: Vec::new(),
            selected_file: None,
            diff: None,
            hide_whitespace: false,
            confirm_drop: None,
            requested: false,
            detail_generation: 0,
            diff_generation: 0,
            tasks: Vec::new(),
            _subscriptions,
        };
        this.sync(cx);
        this
    }

    /// Asks for the list when it isn't there, and keeps the selection pointing at a stash
    /// that still exists.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.repo_state.read(cx);
        if state.repo().is_none() {
            self.requested = false;
            return;
        }
        let Some(stashes) = state.stashes() else {
            if !self.requested {
                self.requested = true;
                self.repo_state
                    .update(cx, |state, cx| state.load_stashes(cx));
            }
            return;
        };
        self.requested = false;
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|id| stashes.iter().any(|stash| &stash.commit.id == id));
        if !still_there {
            let first = stashes.first().map(|stash| stash.commit.id.clone());
            self.confirm_drop = None;
            self.select(first, cx);
        }
        cx.notify();
    }

    fn stashes(&self, cx: &gpui::App) -> Vec<Stash> {
        self.repo_state
            .read(cx)
            .stashes()
            .map(<[Stash]>::to_vec)
            .unwrap_or_default()
    }

    fn selected_stash(&self, cx: &gpui::App) -> Option<Stash> {
        let id = self.selected.as_ref()?;
        self.stashes(cx)
            .into_iter()
            .find(|stash| &stash.commit.id == id)
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        self.confirm_drop = None;
        self.files.clear();
        self.selected_file = None;
        self.diff = None;
        self.detail_generation += 1;
        let (Some(repo), Some(stash)) = (
            self.repo_state.read(cx).repo().cloned(),
            self.selected_stash(cx),
        ) else {
            cx.notify();
            return;
        };
        let generation = self.detail_generation;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.stash_files(&stash) })
                .await;
            this.update(cx, |this, cx| {
                if this.detail_generation != generation {
                    return;
                }
                match result {
                    Ok(files) => {
                        this.files = files;
                        if !this.files.is_empty() {
                            this.select_file(0, cx);
                        }
                    }
                    Err(error) => this.diff = Some(DiffContent::Failed(error.to_string().into())),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select_file(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_file = Some(index);
        self.diff = None;
        self.diff_generation += 1;
        self.load_diff(cx);
        cx.notify();
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(stash), Some(file)) = (
            self.repo_state.read(cx).repo().cloned(),
            self.selected_stash(cx),
            self.selected_file
                .and_then(|ix| self.files.get(ix).cloned()),
        ) else {
            return;
        };
        let options = DiffOptions {
            ignore_whitespace: self.hide_whitespace,
        };
        let generation = self.diff_generation;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.stash_diff(&stash, &file, options) })
                .await;
            this.update(cx, |this, cx| {
                if this.diff_generation != generation {
                    return;
                }
                this.diff = Some(match result {
                    Ok(diff) => DiffContent::from_diffs(
                        &FileDiffs {
                            staged: git_core::FileDiff::default(),
                            unstaged: diff,
                        },
                        HunkControls::default(),
                    ),
                    Err(error) => DiffContent::Failed(error.to_string().into()),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn stash_now(&mut self, cx: &mut Context<Self>) {
        let text = self.note.read(cx).value().to_string();
        self.repo_state
            .update(cx, |state, cx| state.stash_changes(text, cx));
    }

    fn apply(&mut self, pop: bool, cx: &mut Context<Self>) {
        if let Some(stash) = self.selected_stash(cx) {
            self.repo_state.update(cx, |state, cx| {
                if pop {
                    state.pop_stash(stash, cx)
                } else {
                    state.apply_stash(stash, cx)
                }
            });
        }
    }

    fn drop_selected(&mut self, cx: &mut Context<Self>) {
        let Some(stash) = self.selected_stash(cx) else {
            return;
        };
        if self.confirm_drop.as_ref() == Some(&stash.commit.id) {
            self.confirm_drop = None;
            self.repo_state
                .update(cx, |state, cx| state.drop_stash(stash, cx));
        } else {
            self.confirm_drop = Some(stash.commit.id);
        }
        cx.notify();
    }

    fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        self.hide_whitespace = !self.hide_whitespace;
        self.diff = None;
        self.diff_generation += 1;
        self.load_diff(cx);
        cx.notify();
    }

    fn detail(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(stash) = self.selected_stash(cx) else {
            return message(theme, "Select a stash to see what it holds");
        };
        let button = |id: &'static str| {
            div()
                .id(id)
                .h(px(36.))
                .px(px(16.))
                .flex()
                .items_center()
                .rounded(px(18.))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|b| b.bg(theme.surface_selected))
        };
        let confirming = self.confirm_drop.as_ref() == Some(&stash.commit.id);
        let busy = self.repo_state.read(cx).is_busy();

        let header =
            div()
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
                                .text_size(px(16.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .truncate()
                                .child(SharedString::from(stash.message.clone())),
                        )
                        .child(div().text_size(px(12.)).text_color(theme.text_muted).child(
                            format!(
                                "stash@{{{}}} on {} · {}",
                                stash.index,
                                stash.branch,
                                relative_time(stash.commit.time, now())
                            ),
                        )),
                )
                .child(
                    button("apply-stash")
                        .when(busy, |b| b.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.apply(false, cx)))
                        .child("Apply"),
                )
                .child(
                    button("pop-stash")
                        .bg(theme.accent)
                        .border_color(theme.accent)
                        .text_color(theme.accent_text)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .when(busy, |b| b.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.apply(true, cx)))
                        .child("Pop"),
                )
                .child(
                    button("drop-stash")
                        .border_color(theme.error_border)
                        .text_color(theme.error_text)
                        .when(confirming, |b| b.bg(theme.error_background))
                        .on_click(cx.listener(|this, _, _, cx| this.drop_selected(cx)))
                        .child(if confirming {
                            "Click again to drop"
                        } else {
                            "Drop"
                        }),
                );

        let file_rows: Vec<AnyElement> = self
            .files
            .iter()
            .enumerate()
            .map(|(ix, entry)| {
                let file = &entry.file;
                let selected = self.selected_file == Some(ix);
                let name = file
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file.path)
                    .to_owned();
                div()
                    .id(("stash-file", ix))
                    .w_full()
                    .h(px(38.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.surface_selected))
                    .hover(|row| row.bg(theme.surface_selected))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_file(ix, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    )
                    .when(entry.untracked, |row| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_muted)
                                .child("new"),
                        )
                    })
                    .children(file.additions.map(|n| {
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(11.))
                            .text_color(theme.added.text)
                            .child(format!("+{n}"))
                    }))
                    .children(file.deletions.map(|n| {
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(11.))
                            .text_color(theme.deleted.text)
                            .child(format!("−{n}"))
                    }))
                    .into_any_element()
            })
            .collect();

        let hide_whitespace = self.hide_whitespace;
        let body = diff_body(
            self.diff.as_ref(),
            ("stash-diff", self.diff_generation as usize),
            hide_whitespace,
            theme,
            DiffInteraction::read_only(),
        );

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(240.))
                            .flex_none()
                            .border_r_1()
                            .border_color(theme.border)
                            .children(file_rows),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .h(px(36.))
                                    .flex_none()
                                    .px(px(14.))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .id("stash-whitespace")
                                            .px(px(10.))
                                            .h(px(26.))
                                            .flex()
                                            .items_center()
                                            .rounded(px(13.))
                                            .cursor_pointer()
                                            .text_size(px(12.))
                                            .text_color(if hide_whitespace {
                                                theme.accent_soft
                                            } else {
                                                theme.text_muted
                                            })
                                            .hover(|b| b.bg(theme.surface_selected))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_whitespace(cx)
                                            }))
                                            .child(if hide_whitespace {
                                                "Whitespace hidden"
                                            } else {
                                                "Hide whitespace"
                                            }),
                                    ),
                            )
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

impl Render for StashView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let has_repo = self.repo_state.read(cx).repo().is_some();
        let loaded = self.repo_state.read(cx).stashes().is_some();
        let stashes = self.stashes(cx);
        let busy = self.repo_state.read(cx).is_busy();

        let list: AnyElement = if !has_repo {
            message(&theme, "No repository open")
        } else if !loaded {
            message(&theme, "Loading stashes…")
        } else if stashes.is_empty() {
            message(&theme, "No stashes yet")
        } else {
            div()
                .flex_1()
                .min_h_0()
                .py(px(6.))
                .children(stashes.iter().map(|stash| {
                    let id = stash.commit.id.clone();
                    let selected = self.selected.as_ref() == Some(&id);
                    div()
                        .id(("stash", stash.index))
                        .h(px(68.))
                        .px(px(14.))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap(px(3.))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(theme.surface_selected))
                        .hover(|row| row.bg(theme.surface_selected))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select(Some(id.clone()), cx)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .font_family(MONO_FONT)
                                        .text_size(px(11.))
                                        .text_color(theme.text_muted)
                                        .child(format!("{{{}}}", stash.index)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(SharedString::from(stash.message.clone())),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(format!(
                                    "on {} · {}",
                                    stash.branch,
                                    relative_time(stash.commit.time, now())
                                )),
                        )
                }))
                .into_any_element()
        };

        let panel = |theme: &Theme| {
            div()
                .flex()
                .flex_col()
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .rounded(px(12.))
                .overflow_hidden()
        };
        let detail = self.detail(&theme, cx);

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(12.))
            .px(px(16.))
            .pb(px(12.))
            .child(
                panel(&theme)
                    .w(px(360.))
                    .flex_none()
                    .child(
                        div()
                            .h(px(48.))
                            .flex_none()
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Stashes"),
                            )
                            .child(
                                div()
                                    .px(px(9.))
                                    .py(px(2.))
                                    .rounded(px(10.))
                                    .bg(theme.pill_active)
                                    .text_size(px(11.5))
                                    .text_color(theme.text_muted)
                                    .child(stashes.len().to_string()),
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
                            .child(Input::new(&self.note).aria_label("Stash message"))
                            .child(
                                div()
                                    .id("stash-changes")
                                    .h(px(40.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(10.))
                                    .bg(theme.accent)
                                    .text_color(theme.accent_text)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .when(!has_repo || busy, |b| b.opacity(0.5))
                                    .on_click(cx.listener(|this, _, _, cx| this.stash_now(cx)))
                                    .child("Stash current changes"),
                            ),
                    ),
            )
            .child(panel(&theme).flex_1().min_w_0().child(detail))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use tempfile::TempDir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@e.com"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// A trusted repo with two stashes, "older" then "newer", and a clean tree.
    fn open<'a>(
        cx: &'a mut TestAppContext,
        dir: &Path,
    ) -> (
        Entity<RepoState>,
        Entity<StashView>,
        &'a mut VisualTestContext,
    ) {
        git(dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "base"]);
        fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["stash", "push", "-q", "-m", "older"]);
        fs::write(dir.join("a.txt"), "one\nthree\n").unwrap();
        fs::write(dir.join("fresh.txt"), "new\n").unwrap();
        git(
            dir,
            &["stash", "push", "-q", "--include-untracked", "-m", "newer"],
        );

        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.to_owned(), cx));
        cx.run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        let (view, cx) = cx.add_window_view({
            let state = state.clone();
            move |window, cx| StashView::new(state, window, cx)
        });
        cx.run_until_parked();
        (state, view, cx)
    }

    #[gpui::test]
    fn it_lists_the_stashes_and_shows_the_newest(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());

        let stashes = state.read_with(cx, |s, _| s.stashes().unwrap().to_vec());
        assert_eq!(
            stashes
                .iter()
                .map(|s| s.message.as_str())
                .collect::<Vec<_>>(),
            ["newer", "older"]
        );
        view.read_with(cx, |v, _| {
            assert_eq!(v.selected.as_ref(), Some(&stashes[0].commit.id));
            let paths: Vec<_> = v.files.iter().map(|f| f.file.path.as_str()).collect();
            assert_eq!(paths, ["a.txt", "fresh.txt"]);
            assert!(v.diff.is_some());
        });
    }

    #[gpui::test]
    fn choosing_another_stash_shows_its_files(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());
        let older = state.read_with(cx, |s, _| s.stashes().unwrap()[1].commit.id.clone());

        view.update(cx, |v, cx| v.select(Some(older.clone()), cx));
        cx.run_until_parked();

        view.read_with(cx, |v, _| {
            assert_eq!(v.selected.as_ref(), Some(&older));
            let paths: Vec<_> = v.files.iter().map(|f| f.file.path.as_str()).collect();
            assert_eq!(paths, ["a.txt"]);
        });
    }

    #[gpui::test]
    fn dropping_needs_a_second_click_and_then_moves_on_to_the_next_stash(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());

        view.update(cx, |v, cx| v.drop_selected(cx));
        cx.run_until_parked();
        assert_eq!(state.read_with(cx, |s, _| s.stashes().unwrap().len()), 2);

        view.update(cx, |v, cx| v.drop_selected(cx));
        cx.run_until_parked();

        let left = state.read_with(cx, |s, _| s.stashes().unwrap().to_vec());
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, "older");
        view.read_with(cx, |v, _| {
            assert_eq!(v.selected.as_ref(), Some(&left[0].commit.id))
        });
    }

    #[gpui::test]
    fn changing_the_selection_cancels_a_pending_drop(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());
        let older = state.read_with(cx, |s, _| s.stashes().unwrap()[1].commit.id.clone());

        view.update(cx, |v, cx| v.drop_selected(cx));
        view.update(cx, |v, cx| v.select(Some(older), cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| v.drop_selected(cx));
        cx.run_until_parked();

        // The click after switching only asks again; nothing is gone yet.
        assert_eq!(state.read_with(cx, |s, _| s.stashes().unwrap().len()), 2);
    }

    #[gpui::test]
    fn stashing_from_the_tab_adds_to_the_list_and_selects_it(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());
        fs::write(dir.path().join("a.txt"), "edited again\n").unwrap();

        cx.update(|window, cx| {
            let note = view.read(cx).note.clone();
            note.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.simulate_input("third");
        view.update(cx, |v, cx| v.stash_now(cx));
        cx.run_until_parked();

        let stashes = state.read_with(cx, |s, _| s.stashes().unwrap().to_vec());
        assert_eq!(stashes.len(), 3);
        assert_eq!(stashes[0].message, "third");
    }
}
