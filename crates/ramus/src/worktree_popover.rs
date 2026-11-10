use std::ops::Range;

use git_core::{Head, Worktree};
use gpui::{
    AnyElement, AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
};

const ROW_HEIGHT: f32 = 40.;

/// One line of the list: a section title or a worktree (an index into the shown ones).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Section(&'static str),
    Worktree(usize),
}

/// The worktree switcher: a filter, then the main worktree and the linked ones.
pub struct WorktreePopover {
    repo_state: Entity<RepoState>,
    filter: Entity<InputState>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for WorktreePopover {}

impl Focusable for WorktreePopover {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The name a worktree goes by: its folder's name.
fn folder_name(worktree: &Worktree) -> String {
    worktree
        .path
        .file_name()
        .unwrap_or(worktree.path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// What a worktree has checked out: its branch, or the short commit when detached.
fn head_label(worktree: &Worktree) -> &str {
    match &worktree.head {
        Head::Branch(name) => name,
        Head::Detached(commit) => commit,
    }
}

/// Whether `worktree` matches what the user typed: every word, in any order, ignoring
/// case, against the folder name or what is checked out there.
fn matches_filter(worktree: &Worktree, filter: &str) -> bool {
    let folder = folder_name(worktree).to_lowercase();
    let head = head_label(worktree).to_lowercase();
    filter.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        folder.contains(&word) || head.contains(&word)
    })
}

/// The rows to show for `worktrees` under `filter`: the main worktree, then the linked
/// ones, each under a title that appears only if it has something under it. Returns the
/// worktrees that survived the filter alongside the rows that index into them.
fn rows_for(worktrees: &[Worktree], filter: &str) -> (Vec<Worktree>, Vec<Row>) {
    let mut shown = Vec::new();
    let mut rows = Vec::new();
    for (title, is_main) in [("MAIN WORKTREE", true), ("LINKED WORKTREES", false)] {
        let matching: Vec<_> = worktrees
            .iter()
            .filter(|worktree| worktree.is_main == is_main && matches_filter(worktree, filter))
            .collect();
        if matching.is_empty() {
            continue;
        }
        rows.push(Row::Section(title));
        for worktree in matching {
            rows.push(Row::Worktree(shown.len()));
            shown.push(worktree.clone());
        }
    }
    (shown, rows)
}

impl WorktreePopover {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        filter.update(cx, |input, cx| input.focus(window, cx));

        let _subscriptions = vec![
            cx.observe(&repo_state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&filter, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];

        Self {
            repo_state,
            filter,
            focus_handle: cx.focus_handle(),
            _subscriptions,
        }
    }

    fn pick(&mut self, worktree: Worktree, cx: &mut Context<Self>) {
        self.repo_state.update(cx, |state, cx| {
            if !state.is_open_worktree(&worktree) {
                state.switch_worktree(&worktree, cx);
            }
        });
        cx.emit(DismissEvent);
    }

    fn worktree_row(
        &self,
        ix: usize,
        worktree: &Worktree,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.repo_state.read(cx).is_open_worktree(worktree);
        let note = |text: &'static str| {
            div()
                .flex_none()
                .px(px(8.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(10.))
                .bg(theme.pill_active)
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child(text)
        };
        div()
            .id(("worktree", ix))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .when(is_open, |row| row.bg(theme.surface_selected))
            // A worktree whose folder is gone can't be opened, so it isn't clickable.
            .when(worktree.missing, |row| row.text_color(theme.text_muted))
            .when(!worktree.missing, |row| {
                let view = cx.entity();
                let picked = worktree.clone();
                row.cursor_pointer()
                    .hover(|row| row.bg(theme.surface_selected))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.pick(picked.clone(), cx))
                    })
            })
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .text_color(theme.accent_soft)
                    .child(if is_open { "✓" } else { "" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(folder_name(worktree))),
            )
            .when(worktree.missing, |row| row.child(note("Folder missing")))
            .when(worktree.locked, |row| row.child(note("Locked")))
            .child(
                div()
                    .flex_shrink(1.)
                    .min_w_0()
                    .max_w(px(200.))
                    .truncate()
                    .font_family(MONO_FONT)
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(head_label(worktree).to_owned())),
            )
            .into_any_element()
    }
}

impl Render for WorktreePopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let filter = self.filter.read(cx).value().to_string();
        let worktrees = self.repo_state.read(cx).worktrees().unwrap_or_default();
        let (shown, rows) = rows_for(worktrees, &filter);

        let body: AnyElement = if rows.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_muted)
                .child(if filter.trim().is_empty() {
                    "No worktrees to show"
                } else {
                    "No worktrees match"
                })
                .into_any_element()
        } else {
            let row_theme = theme.clone();
            uniform_list(
                "worktrees",
                rows.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|ix| match rows.get(ix)? {
                            Row::Section(title) => Some(
                                div()
                                    .id(("section", ix))
                                    .w_full()
                                    .h(px(ROW_HEIGHT))
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .text_size(px(11.))
                                    .text_color(row_theme.text_muted)
                                    .child(*title)
                                    .into_any_element(),
                            ),
                            Row::Worktree(shown_ix) => {
                                Some(this.worktree_row(ix, shown.get(*shown_ix)?, &row_theme, cx))
                            }
                        })
                        .collect()
                }),
            )
            .flex_1()
            .into_any_element()
        };

        div()
            .id("worktree-popover")
            .track_focus(&self.focus_handle)
            .w(px(440.))
            .h(px(560.))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(12.))
            .shadow_lg()
            .overflow_hidden()
            // Clicks inside must not reach the backdrop that closes the popover.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .p(px(12.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Input::new(&self.filter).aria_label("Filter worktrees")),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use git_core::Repo;
    use tempfile::TempDir;

    use super::*;
    use crate::testutil::git;

    /// The worktrees of a new repository at `<parent>/main` with one linked worktree per
    /// `(folder, branch)` in `linked`, each a sibling of `main`.
    fn worktrees_of_repo_with(linked: &[(&str, &str)]) -> (TempDir, Vec<Worktree>) {
        let parent = TempDir::new().unwrap();
        let main = parent.path().join("main");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        fs::write(main.join("a.txt"), "a\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "base"]);
        for (folder, branch) in linked {
            let path = format!("../{folder}");
            git(&main, &["worktree", "add", "-q", "-b", branch, &path]);
        }
        let worktrees = Repo::open(&main).unwrap().worktrees().unwrap();
        (parent, worktrees)
    }

    fn folders(shown: &[Worktree]) -> Vec<String> {
        shown.iter().map(folder_name).collect()
    }

    #[test]
    fn main_and_linked_worktrees_go_under_their_own_titles() {
        let (_parent, worktrees) = worktrees_of_repo_with(&[("one", "one"), ("two", "two")]);

        let (shown, rows) = rows_for(&worktrees, "");

        assert_eq!(folders(&shown), ["main", "one", "two"]);
        assert_eq!(
            rows,
            [
                Row::Section("MAIN WORKTREE"),
                Row::Worktree(0),
                Row::Section("LINKED WORKTREES"),
                Row::Worktree(1),
                Row::Worktree(2),
            ]
        );
    }

    #[test]
    fn the_filter_matches_the_branch_as_well_as_the_folder() {
        let (_parent, worktrees) = worktrees_of_repo_with(&[
            ("jovial-haslett-d49201", "feature/neo-3105-keep"),
            ("other", "other"),
        ]);

        let (by_branch, _) = rows_for(&worktrees, "NEO 3105");
        let (by_folder, _) = rows_for(&worktrees, "jovial");

        assert_eq!(folders(&by_branch), ["jovial-haslett-d49201"]);
        assert_eq!(folders(&by_folder), ["jovial-haslett-d49201"]);
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_no_rows() {
        let (_parent, worktrees) = worktrees_of_repo_with(&[("one", "one")]);

        let (shown, rows) = rows_for(&worktrees, "zzz");

        assert!(shown.is_empty() && rows.is_empty());
    }

    #[test]
    fn the_main_title_disappears_when_the_filter_hides_the_main_worktree() {
        let (_parent, worktrees) = worktrees_of_repo_with(&[("one", "one")]);

        let (shown, rows) = rows_for(&worktrees, "one");

        assert_eq!(folders(&shown), ["one"]);
        assert_eq!(rows, [Row::Section("LINKED WORKTREES"), Row::Worktree(0)]);
    }
}
