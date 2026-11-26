use std::ops::Range;

use git_core::{Branch, relative_time};
use gpui::{
    AnyElement, AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div, prelude::FluentBuilder,
    px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    ui::icon,
};

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
    rename_input: Entity<InputState>,
    /// The local branch whose name is being edited.
    renaming: Option<String>,
    /// The branch a first Delete click asked about, with its unmerged commits once counted.
    deleting: Option<(String, Option<usize>)>,
    delete_check: Option<Task<()>>,
    /// The row under the pointer, which shows Rename and Delete.
    hovered: Option<usize>,
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
/// Only a local branch can be deleted, and not the one checked out here or in another
/// worktree: git refuses those.
fn can_delete(branch: &Branch, held_elsewhere: bool) -> bool {
    branch.kind == git_core::BranchKind::Local && !branch.is_current && !held_elsewhere
}

/// What deleting a branch with `unmerged` commits nowhere else would lose, or `None` when
/// nothing would be lost and it can go without asking.
fn delete_warning(unmerged: usize) -> Option<String> {
    match unmerged {
        0 => None,
        1 => Some("1 unmerged commit".to_owned()),
        count => Some(format!("{count} unmerged commits")),
    }
}

/// The tag shown on a branch another worktree has checked out: that worktree's folder name.
fn worktree_tag(path: &std::path::Path) -> SharedString {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
        .into()
}

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
        let rename_input = cx.new(|cx| InputState::new(window, cx));
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
            cx.subscribe_in(
                &rename_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.finish_rename(window, cx);
                    }
                },
            ),
        ];

        Self {
            repo_state,
            filter,
            new_name,
            creating: false,
            rename_input,
            renaming: None,
            deleting: None,
            delete_check: None,
            hovered: None,
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
        // Picking this branch opens the worktree that has it, so the row says which one.
        let held_by = self
            .repo_state
            .read(cx)
            .worktree_holding(branch)
            .map(|worktree| worktree_tag(&worktree.path));
        let local = branch.kind == git_core::BranchKind::Local;
        let renaming = local && self.renaming.as_deref() == Some(branch.name.as_str());
        let deleting = self
            .deleting
            .as_ref()
            .filter(|(name, _)| *name == branch.name)
            .map(|(_, unmerged)| *unmerged);
        let warning = deleting.flatten().and_then(delete_warning);
        let show_actions = local && !renaming && (self.hovered == Some(ix) || deleting.is_some());
        let deletable = can_delete(branch, held_by.is_some());
        let link = |id: (&'static str, usize)| {
            div()
                .id(id)
                .px(px(8.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .text_size(px(theme.metrics.text_sm))
                .text_color(theme.accent_soft)
                .hover(|link| link.bg(theme.pill_active))
                // An action on the row must not also pick the branch the row stands for.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        div()
            .id(("branch", ix))
            .w_full()
            .h(px(theme.metrics.row_height))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(branch.is_current, |row| row.bg(theme.surface_selected))
            .hover(|row| row.bg(theme.surface_selected))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(ix);
                } else if this.hovered == Some(ix) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .when(!renaming, |row| {
                row.on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.pick(picked.clone(), cx))
                })
            })
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .text_color(theme.accent_soft)
                    .when(branch.is_current, |mark| {
                        mark.child(icon("check", 14., theme.accent_soft))
                    }),
            )
            .when(renaming, |row| {
                row.child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.rename_input)
                            .h(px(24.))
                            .aria_label("New name for the branch"),
                    ),
                )
            })
            .when(!renaming, |row| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO_FONT)
                        .child(SharedString::from(branch.name.clone())),
                )
            })
            .children(warning.clone().map(|warning| {
                div()
                    .flex_none()
                    .text_size(px(theme.metrics.text_sm))
                    .text_color(theme.error_text)
                    .child(warning)
            }))
            .when(show_actions, |row| {
                let (rename, delete) = (branch.name.clone(), branch.name.clone());
                row.child(
                    link(("rename", ix))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.start_rename(rename.clone(), window, cx);
                        }))
                        .child("Rename"),
                )
                .when(deletable, |row| {
                    row.child(
                        link(("delete", ix))
                            .text_color(theme.error_text)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.ask_delete(delete.clone(), cx);
                            }))
                            .child(if warning.is_some() {
                                "Delete anyway"
                            } else {
                                "Delete"
                            }),
                    )
                })
            })
            .when(!renaming && !branch.is_current && local, |row| {
                let view = cx.entity();
                let name = branch.name.clone();
                row.child(
                    link(("merge", ix))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            view.update(cx, |this, cx| this.merge(name.clone(), cx))
                        })
                        .child("Merge"),
                )
            })
            .children(held_by.filter(|_| !show_actions).map(|folder| {
                div()
                    .flex_none()
                    .max_w(px(140.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(theme.metrics.text_sm))
                    .text_color(theme.text_muted)
                    .child(icon("worktree", 12., theme.text_muted))
                    .child(div().min_w_0().truncate().child(folder))
            }))
            .children(
                branch
                    .last_commit
                    .filter(|_| !show_actions && !renaming)
                    .map(|time| {
                        div()
                            .flex_none()
                            .text_size(px(theme.metrics.text_sm))
                            .text_color(theme.text_muted)
                            .child(relative_time(time, now))
                    }),
            )
            .into_any_element()
    }

    /// Turns the branch's row into a name box, filled with its current name.
    fn start_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.deleting = None;
        self.rename_input.update(cx, |input, cx| {
            input.set_value(name.clone(), window, cx);
            input.focus(window, cx);
        });
        self.renaming = Some(name);
        cx.notify();
    }

    /// Saves the name typed in the box; an empty or unchanged name just closes it. Typing
    /// goes back to the filter, since the box is gone.
    fn finish_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(old) = self.renaming.take() else {
            return;
        };
        self.filter.update(cx, |input, cx| input.focus(window, cx));
        let new = self.rename_input.read(cx).value().trim().to_owned();
        if !new.is_empty() && new != old {
            self.repo_state
                .update(cx, |state, cx| state.rename_branch(old, new, cx));
        }
        cx.notify();
    }

    /// A first click counts what deleting `name` would lose: nothing, and it goes at once;
    /// otherwise the row warns and a second click deletes.
    fn ask_delete(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some((asked, Some(unmerged))) = &self.deleting
            && *asked == name
            && *unmerged > 0
        {
            self.deleting = None;
            self.repo_state
                .update(cx, |state, cx| state.delete_branch(name, cx));
            return;
        }
        let Some(repo) = self.repo_state.read(cx).repo().cloned() else {
            return;
        };
        self.deleting = Some((name.clone(), None));
        let counted_for = name.clone();
        self.delete_check = Some(cx.spawn(async move |this, cx| {
            let counted = cx
                .background_executor()
                .spawn(async move { repo.unmerged_commits(&counted_for) })
                .await;
            this.update(cx, |this, cx| {
                // Another row was asked about while this one was being counted.
                if this.deleting.as_ref().map(|(asked, _)| asked) != Some(&name) {
                    return;
                }
                match counted {
                    Ok(0) => {
                        this.deleting = None;
                        this.repo_state
                            .update(cx, |state, cx| state.delete_branch(name, cx));
                    }
                    Ok(unmerged) => this.deleting = Some((name, Some(unmerged))),
                    Err(error) => {
                        this.deleting = None;
                        this.repo_state
                            .update(cx, |state, cx| state.report(error.to_string(), cx));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
                                            .h(px(theme.metrics.row_height))
                                            .px(px(12.))
                                            .flex()
                                            .items_center()
                                            .text_size(px(theme.metrics.text_xs))
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
            .rounded(px(theme.metrics.radius_lg))
            .shadow_lg()
            .overflow_hidden()
            // Clicks inside must not reach the backdrop that closes the popover.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                // Escape backs out of a rename or a pending delete before it closes the list.
                if this.renaming.take().is_some() {
                    this.filter.update(cx, |input, cx| input.focus(window, cx));
                    cx.notify();
                } else if this.deleting.take().is_some() {
                    cx.notify();
                } else {
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
                                .h(px(theme.metrics.control_height))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme.metrics.radius_md))
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
                                        .h(px(theme.metrics.control_height))
                                        .aria_label("New branch name"),
                                ),
                            )
                            .child(
                                div()
                                    .id("create-branch")
                                    .h(px(theme.metrics.control_height))
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(theme.metrics.radius_md))
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
    fn only_a_local_branch_nobody_has_checked_out_can_be_deleted() {
        assert!(can_delete(&local("old"), false));
        assert!(!can_delete(&local("old"), true), "another worktree has it");
        assert!(!can_delete(&remote("origin/old"), false));
        let mut current = local("main");
        current.is_current = true;
        assert!(!can_delete(&current, false));
    }

    #[test]
    fn deleting_asks_first_only_when_commits_would_be_lost() {
        assert_eq!(delete_warning(0), None);
        assert_eq!(delete_warning(1).as_deref(), Some("1 unmerged commit"));
        assert_eq!(delete_warning(3).as_deref(), Some("3 unmerged commits"));
    }

    #[test]
    fn a_worktree_tag_names_the_worktree_folder() {
        assert_eq!(
            worktree_tag(std::path::Path::new("/work/ramus-feature")),
            "ramus-feature"
        );
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_no_rows() {
        let (shown, rows) = rows_for(&[local("main")], &[remote("origin/x")], "zzz");
        assert!(shown.is_empty() && rows.is_empty());
    }
}
