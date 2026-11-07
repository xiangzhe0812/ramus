use std::ops::Range;

use git_core::{Branch, relative_time};
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

/// One line of the list: a section title or a branch (an index into the shown branches).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Section(&'static str),
    Branch(usize),
}

/// The branch switcher: a filter, the branches newest first, and a way to make a new one.
pub struct BranchPopover {
    repo_state: Entity<RepoState>,
    filter: Entity<InputState>,
    new_name: Entity<InputState>,
    creating: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for BranchPopover {}

impl Focusable for BranchPopover {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Whether `branch` matches what the user typed: every word, in any order, ignoring case.
fn matches_filter(branch: &Branch, filter: &str) -> bool {
    let name = branch.name.to_lowercase();
    filter
        .split_whitespace()
        .all(|word| name.contains(&word.to_lowercase()))
}

/// The rows to show for `branches` under `filter`: local branches, then remote-only
/// ones, each under a title that appears only if it has something under it. Returns the
/// branches that survived the filter alongside the rows that index into them.
fn rows_for(local: &[Branch], remote_only: &[Branch], filter: &str) -> (Vec<Branch>, Vec<Row>) {
    let mut shown = Vec::new();
    let mut rows = Vec::new();
    for (title, branches) in [("RECENT BRANCHES", local), ("REMOTE BRANCHES", remote_only)] {
        let matching: Vec<_> = branches
            .iter()
            .filter(|branch| matches_filter(branch, filter))
            .collect();
        if matching.is_empty() {
            continue;
        }
        rows.push(Row::Section(title));
        for branch in matching {
            rows.push(Row::Branch(shown.len()));
            shown.push(branch.clone());
        }
    }
    (shown, rows)
}

impl BranchPopover {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        repo_state.update(cx, |state, cx| state.load_branches(cx));

        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let new_name = cx.new(|cx| InputState::new(window, cx).placeholder("New branch name"));
        filter.update(cx, |input, cx| input.focus(window, cx));

        let _subscriptions = vec![
            cx.observe(&repo_state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&filter, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&new_name, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.create(cx);
                }
            }),
        ];

        Self {
            repo_state,
            filter,
            new_name,
            creating: false,
            focus_handle: cx.focus_handle(),
            _subscriptions,
        }
    }

    fn pick(&mut self, branch: Branch, cx: &mut Context<Self>) {
        if !branch.is_current {
            self.repo_state
                .update(cx, |state, cx| state.switch_branch(branch, cx));
        }
        cx.emit(DismissEvent);
    }

    /// Merges `branch` into the current branch. A conflict aborts the merge.
    fn merge(&mut self, branch: String, cx: &mut Context<Self>) {
        self.repo_state
            .update(cx, |state, cx| state.merge_branch(branch, cx));
        cx.emit(DismissEvent);
    }

    pub fn start_creating(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating = true;
        self.new_name
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.repo_state
            .update(cx, |state, cx| state.create_branch(name, cx));
        cx.emit(DismissEvent);
    }

    fn branch_row(
        &self,
        ix: usize,
        branch: &Branch,
        now: i64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let picked = branch.clone();
        div()
            .id(("branch", ix))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(branch.is_current, |row| row.bg(theme.surface_selected))
            .hover(|row| row.bg(theme.surface_selected))
            .on_click(move |_, _, cx| view.update(cx, |this, cx| this.pick(picked.clone(), cx)))
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .text_color(theme.accent_soft)
                    .child(if branch.is_current { "✓" } else { "" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO_FONT)
                    .child(SharedString::from(branch.name.clone())),
            )
            .when(
                !branch.is_current && branch.kind == git_core::BranchKind::Local,
                |row| {
                    let view = cx.entity();
                    let name = branch.name.clone();
                    row.child(
                        div()
                            .id(("merge", ix))
                            .px(px(8.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .rounded(px(12.))
                            .text_size(px(11.5))
                            .text_color(theme.accent_soft)
                            .hover(|link| link.bg(theme.pill_active))
                            // Merging must not also switch to the branch the row stands for.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                view.update(cx, |this, cx| this.merge(name.clone(), cx))
                            })
                            .child("Merge"),
                    )
                },
            )
            .children(branch.last_commit.map(|time| {
                div()
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(relative_time(time, now))
            }))
            .into_any_element()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

impl Render for BranchPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let filter = self.filter.read(cx).value().to_string();
        let loaded = self.repo_state.read(cx).branches().cloned();

        let body: AnyElement = match loaded {
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_muted)
                .child("Loading branches…")
                .into_any_element(),
            Some(list) => {
                let (shown, rows) = rows_for(&list.local, &list.remote_only, &filter);
                if rows.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.text_muted)
                        .child(if filter.trim().is_empty() {
                            "No branches yet"
                        } else {
                            "No branches match"
                        })
                        .into_any_element()
                } else {
                    let row_theme = theme.clone();
                    let timestamp = now();
                    uniform_list(
                        "branches",
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
                                    Row::Branch(shown_ix) => Some(this.branch_row(
                                        ix,
                                        shown.get(*shown_ix)?,
                                        timestamp,
                                        &row_theme,
                                        cx,
                                    )),
                                })
                                .collect()
                        }),
                    )
                    .flex_1()
                    .into_any_element()
                }
            }
        };

        div()
            .id("branch-popover")
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
                    .child(Input::new(&self.filter).aria_label("Filter branches")),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .p(px(12.))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    // The name box and the button share one height and one centre line.
                    .items_center()
                    .gap(px(8.))
                    .when(!self.creating, |footer| {
                        footer.child(
                            div()
                                .id("new-branch")
                                .flex_1()
                                .h(px(40.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(8.))
                                .bg(theme.accent)
                                .text_color(theme.accent_text)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_creating(window, cx)
                                }))
                                .child("New branch…"),
                        )
                    })
                    .when(self.creating, |footer| {
                        footer
                            .child(
                                div().flex_1().child(
                                    Input::new(&self.new_name)
                                        .h(px(40.))
                                        .aria_label("New branch name"),
                                ),
                            )
                            .child(
                                div()
                                    .id("create-branch")
                                    .h(px(40.))
                                    .px(px(16.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(8.))
                                    .bg(theme.accent)
                                    .text_color(theme.accent_text)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                                    .child("Create"),
                            )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use git_core::BranchKind;

    use super::*;

    fn branch(name: &str, kind: BranchKind) -> Branch {
        Branch {
            name: name.into(),
            kind,
            is_current: false,
            last_commit: None,
            upstream: None,
        }
    }

    fn local(name: &str) -> Branch {
        branch(name, BranchKind::Local)
    }

    fn remote(name: &str) -> Branch {
        branch(name, BranchKind::Remote)
    }

    #[test]
    fn sections_appear_only_when_they_have_branches() {
        let (_, rows) = rows_for(&[local("main")], &[], "");
        assert_eq!(rows, [Row::Section("RECENT BRANCHES"), Row::Branch(0)]);

        let (_, rows) = rows_for(&[], &[remote("origin/x")], "");
        assert_eq!(rows, [Row::Section("REMOTE BRANCHES"), Row::Branch(0)]);
    }

    #[test]
    fn rows_index_into_the_branches_that_survived_the_filter() {
        let (shown, rows) = rows_for(
            &[local("main"), local("feat/diff"), local("feat/stage")],
            &[remote("origin/feat/remote")],
            "feat",
        );
        let names: Vec<_> = shown.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["feat/diff", "feat/stage", "origin/feat/remote"]);
        assert_eq!(
            rows,
            [
                Row::Section("RECENT BRANCHES"),
                Row::Branch(0),
                Row::Branch(1),
                Row::Section("REMOTE BRANCHES"),
                Row::Branch(2),
            ]
        );
    }

    #[test]
    fn the_filter_ignores_case_and_matches_words_in_any_order() {
        let b = local("Feat/Diff-View");
        assert!(matches_filter(&b, "diff"));
        assert!(matches_filter(&b, "DIFF feat"));
        assert!(matches_filter(&b, "  "));
        assert!(!matches_filter(&b, "stage"));
        assert!(!matches_filter(&b, "feat stage"));
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_no_rows() {
        let (shown, rows) = rows_for(&[local("main")], &[remote("origin/x")], "zzz");
        assert!(shown.is_empty() && rows.is_empty());
    }
}
