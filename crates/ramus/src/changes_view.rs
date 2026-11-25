use std::{
    collections::{BTreeSet, HashSet},
    ops::Range,
    rc::Rc,
};

use git_core::{ChangeKind, DiffOptions, FileChange, FileDiffs};
use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};

use crate::{
    actions::{SelectNextFile, SelectPreviousFile, ToggleSelectedFile},
    diff_rows::{HunkAction, HunkControls, LineRef},
    diff_view::{DiffContent, DiffInteraction, diff_body, message},
    repo_state::{RepoEvent, RepoState, SyncKind},
    theme::{Badge, MONO_FONT, Theme},
    title_bar::{SyncAction, primary_sync},
    ui::panel,
};

/// How much of a file (or of all files) is staged for the next commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inclusion {
    None,
    Some,
    All,
}

struct LoadedDiff {
    /// The raw diffs, kept to skip a redraw when a refresh finds nothing new, and to
    /// build the patch when a hunk is staged.
    source: Option<FileDiffs>,
    /// Whether this file can be staged hunk by hunk at all, whatever the current options.
    stageable: bool,
    /// What the rows were built with; a change here needs a rebuild even if the diff is the same.
    controls: HunkControls,
    content: DiffContent,
}

impl LoadedDiff {
    fn failed(message: impl Into<SharedString>) -> Self {
        Self {
            source: None,
            stageable: false,
            controls: HunkControls::default(),
            content: DiffContent::Failed(message.into()),
        }
    }

    fn from_result(
        result: Result<FileDiffs, git_core::Error>,
        stageable: bool,
        controls: HunkControls,
        path: &str,
    ) -> Self {
        let diffs = match result {
            Ok(diffs) => diffs,
            Err(error) => return Self::failed(error.to_string()),
        };
        let content = DiffContent::from_diffs(&diffs, controls, path);
        Self {
            source: Some(diffs),
            stageable,
            controls,
            content,
        }
    }
}

/// What the Changes tab asks the workspace to do on its behalf.
pub enum ChangesEvent {
    /// The user asked to discard these files; the workspace confirms first.
    DiscardRequested(Vec<FileChange>),
}

/// The Changes tab: the changed-files list and the diff of the selected file.
pub struct ChangesView {
    repo_state: Entity<RepoState>,
    selected: Option<String>,
    diff: Option<LoadedDiff>,
    hide_whitespace: bool,
    /// Bumped per selection so each file's diff list starts scrolled to the top,
    /// and so a slow diff for a previous selection is ignored.
    diff_generation: u64,
    diff_task: Option<Task<()>>,
    summary: Entity<InputState>,
    description: Entity<TextareaState>,
    /// The next commit replaces the last one instead of adding to it.
    amend: bool,
    /// Changed lines picked to be staged or unstaged on their own.
    line_selection: HashSet<LineRef>,
    /// Holds keyboard focus while the file list is in use, so arrow keys move through it.
    list_focus: FocusHandle,
    list_scroll: UniformListScrollHandle,
    /// What was typed before amending filled the message boxes, to put back afterwards.
    draft: Option<(String, String)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let summary = cx.new(|cx| InputState::new(window, cx).placeholder("Summary (required)"));
        let description = cx.new(|cx| TextareaState::new(window, cx).placeholder("Description"));

        let _subscriptions = vec![
            cx.observe(&repo_state, |this, _, cx| this.repo_changed(cx)),
            // The commit button depends on whether the summary is empty.
            cx.subscribe_in(
                &summary,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.commit(cx),
                    _ => cx.notify(),
                },
            ),
            cx.subscribe_in(
                &repo_state,
                window,
                |this, _, event: &RepoEvent, window, cx| match event {
                    RepoEvent::Committed => this.clear_message(window, cx),
                },
            ),
        ];

        Self {
            repo_state,
            selected: None,
            diff: None,
            hide_whitespace: false,
            diff_generation: 0,
            diff_task: None,
            summary,
            description,
            amend: false,
            line_selection: HashSet::new(),
            list_focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            draft: None,
            _subscriptions,
        }
    }

    /// Moves the selection `delta` files down (negative: up), scrolling it into view.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .repo_state
            .read(cx)
            .status()
            .map(|status| status.files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default();
        if paths.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| paths.iter().position(|path| path == selected));
        let next = match current {
            Some(ix) => (ix as isize + delta).clamp(0, paths.len() as isize - 1) as usize,
            None if delta < 0 => paths.len() - 1,
            None => 0,
        };
        self.list_scroll
            .scroll_to_item(next, ScrollStrategy::Nearest);
        self.select(paths[next].clone(), cx);
    }

    /// Stages or unstages the selected file, like clicking its checkbox.
    fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected.as_deref() else {
            return;
        };
        let file = self
            .repo_state
            .read(cx)
            .status()
            .and_then(|status| status.files.iter().find(|file| file.path == path))
            .cloned();
        if let Some(file) = file {
            self.toggle_file(&file, cx);
        }
    }

    /// Asks to discard the selected file.
    pub fn request_discard(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected.as_deref() else {
            return;
        };
        let file = self
            .repo_state
            .read(cx)
            .status()
            .and_then(|status| status.files.iter().find(|file| file.path == path))
            .cloned();
        if let Some(file) = file {
            cx.emit(ChangesEvent::DiscardRequested(vec![file]));
        }
    }

    /// Switches between committing and amending. Amending shows the last commit's message
    /// to edit; switching back restores what was being typed.
    fn toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.amend {
            self.amend = false;
            if let Some((summary, description)) = self.draft.take() {
                self.set_message(&summary, &description, window, cx);
            }
            cx.notify();
            return;
        }
        let Some(repo) = self.repo_state.read(cx).repo().cloned() else {
            return;
        };
        self.amend = true;
        self.draft = Some((
            self.summary.read(cx).value().to_string(),
            self.description.read(cx).value().to_string(),
        ));
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let message = cx
                .background_executor()
                .spawn(async move { repo.last_commit_message() })
                .await;
            this.update_in(cx, |this, window, cx| {
                // Unchecked again, or nothing to amend, while the message was loading.
                if !this.amend {
                    return;
                }
                if let Ok(Some((summary, description))) = message {
                    this.set_message(&summary, &description, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_message(
        &mut self,
        summary: &str,
        description: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (summary, description) = (summary.to_owned(), description.to_owned());
        self.summary
            .update(cx, |input, cx| input.set_value(summary, window, cx));
        self.description
            .update(cx, |input, cx| input.set_value(description, window, cx));
        cx.notify();
    }

    fn clear_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = false;
        self.draft = None;
        self.summary
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.description
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let summary = self.summary.read(cx).value().trim().to_owned();
        let staged = self
            .repo_state
            .read(cx)
            .status()
            .is_some_and(|status| status.files.iter().any(|file| file.is_staged()));
        if summary.is_empty() || (!staged && !self.amend) || !self.repo_state.read(cx).is_trusted()
        {
            return;
        }
        let description = self.description.read(cx).value().to_string();
        let amend = self.amend;
        self.repo_state.update(cx, |state, cx| {
            if amend {
                state.amend(summary, description, cx)
            } else {
                state.commit(summary, description, cx)
            }
        });
    }

    /// Stages the file if anything in it is unstaged, otherwise unstages it.
    fn toggle_file(&mut self, file: &FileChange, cx: &mut Context<Self>) {
        let paths = file.pathspecs().into_iter().map(str::to_owned).collect();
        self.repo_state.update(cx, |state, cx| {
            if file.is_fully_staged() {
                state.unstage(paths, cx)
            } else {
                state.stage(paths, cx)
            }
        });
    }

    fn toggle_all(&mut self, inclusion: Inclusion, cx: &mut Context<Self>) {
        self.repo_state.update(cx, |state, cx| {
            if inclusion == Inclusion::All {
                state.unstage_all(cx)
            } else {
                state.stage_all(cx)
            }
        });
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(path.as_str()) {
            return;
        }
        self.selected = Some(path);
        self.line_selection.clear();
        self.diff = None;
        self.diff_generation += 1;
        self.load_diff(cx);
        cx.notify();
    }

    fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        self.hide_whitespace = !self.hide_whitespace;
        self.line_selection.clear();
        self.load_diff(cx);
        cx.notify();
    }

    /// Status changed: keep the selection if its file is still changed, and re-read its diff.
    fn repo_changed(&mut self, cx: &mut Context<Self>) {
        let still_changed = self.selected.as_ref().is_some_and(|path| {
            self.repo_state
                .read(cx)
                .status()
                .is_some_and(|status| status.files.iter().any(|file| &file.path == path))
        });
        if still_changed {
            self.load_diff(cx);
        } else if self.selected.take().is_some() || self.diff.take().is_some() {
            self.diff_task = None;
            self.diff_generation += 1;
        }
        cx.notify();
    }

    /// Picks or unpicks a changed line to stage (or unstage) on its own.
    fn toggle_line(&mut self, line: LineRef, cx: &mut Context<Self>) {
        // Lines of the staged and unstaged sides can't be mixed in one action.
        self.line_selection
            .retain(|picked| picked.staged == line.staged);
        if !self.line_selection.remove(&line) {
            self.line_selection.insert(line);
        }
        cx.notify();
    }

    /// Stages or unstages a hunk of the selected file's current diff: just the lines picked
    /// in it if there are any, otherwise all of it.
    fn apply_hunk(&mut self, action: HunkAction, cx: &mut Context<Self>) {
        let (Some(path), Some(source)) = (
            self.selected.clone(),
            self.diff.as_ref().and_then(|diff| diff.source.as_ref()),
        ) else {
            return;
        };
        let hunks = if action.staged {
            &source.staged.hunks
        } else {
            &source.unstaged.hunks
        };
        let Some(hunk) = hunks.get(action.index).cloned() else {
            return;
        };
        let picked: BTreeSet<usize> = self
            .line_selection
            .iter()
            .filter(|line| line.staged == action.staged && line.hunk == action.index)
            .map(|line| line.line)
            .collect();
        self.repo_state
            .update(cx, |state, cx| match (action.staged, picked.is_empty()) {
                (true, true) => state.unstage_hunks(path, vec![hunk], cx),
                (false, true) => state.stage_hunks(path, vec![hunk], cx),
                (true, false) => state.unstage_lines(path, hunk, picked, cx),
                (false, false) => state.stage_lines(path, hunk, picked, cx),
            });
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let state = self.repo_state.read(cx);
        let (Some(repo), Some(status), Some(path)) =
            (state.repo().cloned(), state.status(), self.selected.clone())
        else {
            return;
        };
        let Some(change) = status.files.iter().find(|file| file.path == path).cloned() else {
            return;
        };
        if change.kind() == ChangeKind::Conflicted {
            self.diff = Some(LoadedDiff::failed("Merge conflicts can't be viewed yet."));
            return;
        }

        let options = DiffOptions {
            ignore_whitespace: self.hide_whitespace,
        };
        // A diff computed with whitespace ignored has hunks whose context won't match the
        // index, so hunks can't be staged from it.
        let stageable = change.can_stage_hunks() || change.can_unstage_hunks();
        let controls = HunkControls {
            stage: change.can_stage_hunks() && !self.hide_whitespace,
            unstage: change.can_unstage_hunks() && !self.hide_whitespace,
        };
        let generation = self.diff_generation;
        let path = change.path.clone();
        self.diff_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.file_diffs(&change, options) })
                .await;

            this.update(cx, |this, cx| {
                if this.diff_generation != generation {
                    return;
                }
                // A refresh that finds the same diff must not redraw or reset the scroll.
                let unchanged = match (&result, this.diff.as_ref()) {
                    (Ok(new), Some(old)) => {
                        old.source.as_ref() == Some(new)
                            && old.controls == controls
                            && old.stageable == stageable
                    }
                    _ => false,
                };
                if !unchanged {
                    // Picked lines belong to the diff they were picked in.
                    this.line_selection.clear();
                    this.diff = Some(LoadedDiff::from_result(result, stageable, controls, &path));
                    cx.notify();
                }
            })
            .ok();
        }));
    }
}

/// What the clean-tree pane suggests doing about the remote, as a headline and the kind of
/// sync it runs. Fetching isn't suggested: with nothing to send or receive it has no news.
fn clean_tree_next_step(action: &SyncAction) -> Option<(SyncKind, String)> {
    let plural = |n: u32| if n == 1 { "commit" } else { "commits" };
    let count = action.count.unwrap_or(0);
    let headline = match action.kind {
        SyncKind::Push => format!("Push {count} {} to {}", plural(count), action.remote),
        SyncKind::Pull | SyncKind::PullMerge => {
            format!("Pull {count} {} from {}", plural(count), action.remote)
        }
        SyncKind::Publish => format!("Publish this branch to {}", action.remote),
        SyncKind::Fetch | SyncKind::Clone => return None,
    };
    Some((action.kind, headline))
}

/// The line under "No local changes".
fn clean_tree_summary(ahead: u32) -> String {
    let base = "Your working tree matches the last commit.";
    match ahead {
        0 => base.to_owned(),
        1 => format!("{base} 1 commit is waiting to be pushed."),
        n => format!("{base} {n} commits are waiting to be pushed."),
    }
}

/// Splits `src/ui/diff_view.rs` into (`diff_view.rs`, `src/ui`).
fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

fn badge_for(kind: ChangeKind, theme: &Theme) -> (&'static str, Badge) {
    match kind {
        ChangeKind::Modified | ChangeKind::TypeChanged => ("M", theme.modified),
        ChangeKind::Added | ChangeKind::Untracked => ("A", theme.added),
        ChangeKind::Deleted => ("D", theme.deleted),
        ChangeKind::Renamed | ChangeKind::Copied => ("R", theme.renamed),
        ChangeKind::Conflicted => ("!", theme.conflicted),
    }
}

fn checkbox(
    id: impl Into<gpui::ElementId>,
    state: Inclusion,
    theme: &Theme,
    on_toggle: impl Fn(&mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let filled = state != Inclusion::None;
    div()
        .id(id)
        .size(px(16.))
        .flex_none()
        .rounded(px(theme.metrics.radius_sm))
        .border_1()
        .border_color(if filled {
            theme.accent
        } else {
            theme.text_muted
        })
        .when(filled, |checkbox| checkbox.bg(theme.accent))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme.metrics.text_xs))
        .text_color(theme.accent_text)
        .cursor_pointer()
        .child(match state {
            Inclusion::All => "✓",
            Inclusion::Some => "–",
            Inclusion::None => "",
        })
        // Toggling must not also select the row underneath.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_toggle(window, cx)
        })
}

fn file_row(
    ix: usize,
    file: &FileChange,
    selected: bool,
    theme: &Theme,
    on_select: impl Fn(&mut Window, &mut gpui::App) + 'static,
    on_toggle: impl Fn(&mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let (name, dir) = split_path(&file.path);
    let (letter, badge) = badge_for(file.kind(), theme);
    let inclusion = if file.is_fully_staged() {
        Inclusion::All
    } else if file.is_staged() {
        Inclusion::Some
    } else {
        Inclusion::None
    };

    div()
        .id(("file", ix))
        .w_full()
        .h(px(40.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.surface_selected))
        .hover(|row| row.bg(theme.surface_selected))
        .on_click(move |_, window, cx| on_select(window, cx))
        .child(checkbox(("include", ix), inclusion, theme, on_toggle))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(SharedString::from(name.to_owned())),
                )
                .when(!dir.is_empty(), |column| {
                    column.child(
                        div()
                            .truncate()
                            .text_size(px(theme.metrics.text_xs))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(dir.to_owned())),
                    )
                }),
        )
        .child(
            div()
                .size(px(20.))
                .flex_none()
                .rounded(px(theme.metrics.radius_md))
                .bg(badge.background)
                .text_color(badge.text)
                .font_family(MONO_FONT)
                .text_size(px(theme.metrics.text_xs))
                .flex()
                .items_center()
                .justify_center()
                .child(letter),
        )
        .into_any_element()
}

impl ChangesView {
    fn pill(text: String, colors: Badge, theme: &Theme) -> impl IntoElement {
        div()
            .px(px(8.))
            .rounded(px(theme.metrics.radius_md))
            .bg(colors.background)
            .text_color(colors.text)
            .font_family(MONO_FONT)
            .text_size(px(theme.metrics.text_sm))
            .child(text)
    }

    fn diff_pane(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(path) = self.selected.clone() else {
            return message(theme, "Select a file to see its changes");
        };

        let whitespace_blocks_hunks =
            self.hide_whitespace && self.diff.as_ref().is_some_and(|diff| diff.stageable);
        let (additions, deletions) = match self.diff.as_ref().map(|diff| &diff.content) {
            Some(DiffContent::Rows {
                additions,
                deletions,
                ..
            }) => (Some(*additions), Some(*deletions)),
            _ => (None, None),
        };

        let hide_whitespace = self.hide_whitespace;
        let header = div()
            .h(px(40.))
            .flex_none()
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO_FONT)
                    .text_size(px(theme.metrics.text_sm))
                    .child(SharedString::from(path)),
            )
            .children(additions.map(|n| Self::pill(format!("+{n}"), theme.added, theme)))
            .children(deletions.map(|n| Self::pill(format!("−{n}"), theme.deleted, theme)))
            .child(div().flex_1())
            .child(
                div()
                    .id("discard-file")
                    .px(px(10.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .rounded(px(theme.metrics.radius_md))
                    .cursor_pointer()
                    .text_size(px(theme.metrics.text_sm))
                    .text_color(theme.deleted.text)
                    .hover(|button| button.bg(theme.deleted.background))
                    .on_click(cx.listener(|this, _, _, cx| this.request_discard(cx)))
                    .child("Discard file…"),
            )
            .when(whitespace_blocks_hunks, |header| {
                header.child(
                    div()
                        .text_size(px(theme.metrics.text_sm))
                        .text_color(theme.text_muted)
                        .child("Hunks can't be staged while whitespace is hidden"),
                )
            })
            .child(
                div()
                    .id("hide-whitespace")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_whitespace(cx)))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded(px(theme.metrics.radius_sm))
                            .border_1()
                            .border_color(if hide_whitespace {
                                theme.accent
                            } else {
                                theme.text_muted
                            })
                            .when(hide_whitespace, |checkbox| checkbox.bg(theme.accent))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(theme.metrics.text_xs))
                            .text_color(theme.accent_text)
                            .when(hide_whitespace, |checkbox| checkbox.child("✓")),
                    )
                    .child("Hide whitespace"),
            );

        let body = {
            let view = cx.entity();
            let line_view = view.clone();
            diff_body(
                self.diff.as_ref().map(|diff| &diff.content),
                ("diff-rows", self.diff_generation as usize),
                hide_whitespace,
                theme,
                DiffInteraction {
                    on_hunk: Rc::new(move |action, cx| {
                        view.update(cx, |this, cx| this.apply_hunk(action, cx))
                    }),
                    on_line: Rc::new(move |line, cx| {
                        line_view.update(cx, |this, cx| this.toggle_line(line, cx))
                    }),
                    selection: Rc::new(self.line_selection.clone()),
                },
            )
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl Render for ChangesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let files: Vec<FileChange> = self
            .repo_state
            .read(cx)
            .status()
            .map(|status| status.files.clone())
            .unwrap_or_default();
        let count = files.len();
        let staged_count = files.iter().filter(|file| file.is_staged()).count();
        let inclusion = if staged_count == 0 {
            Inclusion::None
        } else if files.iter().all(|file| file.is_fully_staged()) {
            Inclusion::All
        } else {
            Inclusion::Some
        };
        let busy = self.repo_state.read(cx).is_busy();
        let branch_name = self
            .repo_state
            .read(cx)
            .status()
            .map(|status| match &status.branch.head {
                git_core::Head::Branch(name) => name.clone(),
                git_core::Head::Detached(sha) => format!("detached at {sha}"),
            })
            .unwrap_or_default();
        let trusted = self.repo_state.read(cx).is_trusted();
        let has_summary = !self.summary.read(cx).value().trim().is_empty();
        let amending = self.amend;
        let can_commit = (staged_count > 0 || amending) && has_summary && !busy && trusted;
        let (has_commits, already_pushed) =
            self.repo_state
                .read(cx)
                .status()
                .map_or((false, false), |status| {
                    (
                        status.branch.oid.is_some(),
                        status.branch.upstream.is_some() && status.branch.ahead == 0,
                    )
                });
        let commit_label = if !trusted {
            "Trust this repository to commit".to_owned()
        } else if amending {
            match staged_count {
                0 => "Amend last commit".to_owned(),
                1 => "Amend with 1 file".to_owned(),
                n => format!("Amend with {n} files"),
            }
        } else if staged_count == 0 {
            "Include files to commit".to_owned()
        } else {
            format!(
                "Commit {staged_count} {} to {branch_name}",
                if staged_count == 1 { "file" } else { "files" }
            )
        };
        let ahead = self
            .repo_state
            .read(cx)
            .status()
            .map_or(0, |status| status.branch.ahead);
        let next_step = {
            let state = self.repo_state.read(cx);
            state
                .status()
                .and_then(primary_sync)
                .filter(|_| state.has_remote() && state.operation().is_none())
                .and_then(|action| clean_tree_next_step(&action))
        };
        let root = self
            .repo_state
            .read(cx)
            .repo()
            .map(|repo| repo.root().to_owned());

        let list_body = if count == 0 {
            message(&theme, "No changed files")
        } else {
            let row_theme = theme.clone();
            uniform_list(
                "changed-files",
                count,
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|ix| {
                            let file = files.get(ix)?;
                            let selected = this.selected.as_deref() == Some(file.path.as_str());
                            let path = file.path.clone();
                            let view = cx.entity();
                            let toggled = file.clone();
                            let toggle_view = view.clone();
                            Some(file_row(
                                ix,
                                file,
                                selected,
                                &row_theme,
                                move |_, cx| {
                                    view.update(cx, |this, cx| this.select(path.clone(), cx))
                                },
                                move |_, cx| {
                                    toggle_view
                                        .update(cx, |this, cx| this.toggle_file(&toggled, cx))
                                },
                            ))
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .into_any_element()
        };

        let detail = if count == 0 {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(480.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(theme.metrics.text_title))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("No local changes"),
                        )
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child(clean_tree_summary(ahead)),
                        )
                        .child(
                            div()
                                .mt(px(14.))
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .children(next_step.map(|(kind, headline)| {
                                    let repo_state = self.repo_state.clone();
                                    div()
                                        .id("clean-next-step")
                                        .h(px(44.))
                                        .px(px(12.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(theme.metrics.radius_md))
                                        .bg(theme.accent)
                                        .text_color(theme.accent_text)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .cursor_pointer()
                                        .on_click(move |_, _, cx| {
                                            repo_state.update(cx, |state, cx| state.sync(kind, cx))
                                        })
                                        .child(headline)
                                }))
                                .children(root.map(|root| {
                                    div()
                                        .id("show-in-finder")
                                        .h(px(44.))
                                        .px(px(12.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(theme.metrics.radius_md))
                                        .border_1()
                                        .border_color(theme.border)
                                        .bg(theme.background)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .cursor_pointer()
                                        .hover(|b| b.bg(theme.surface_selected))
                                        .on_click(move |_, _, cx| cx.reveal_path(&root))
                                        .child("Show in Finder")
                                })),
                        ),
                )
                .into_any_element()
        } else {
            self.diff_pane(&theme, cx)
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
                    .w(px(330.))
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
                            .when(count > 0, |header| {
                                header.child(checkbox("include-all", inclusion, &theme, {
                                    let view = cx.entity();
                                    move |_, cx| {
                                        view.update(cx, |this, cx| this.toggle_all(inclusion, cx))
                                    }
                                }))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Changes"),
                            )
                            .child(
                                div()
                                    .px(px(9.))
                                    .rounded(px(theme.metrics.radius_md))
                                    .bg(theme.pill_active)
                                    .text_size(px(theme.metrics.text_sm))
                                    .text_color(theme.text_muted)
                                    .child(if count == 0 {
                                        "0 files".to_owned()
                                    } else {
                                        format!("{staged_count} of {count} included")
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("file-list")
                            .key_context("ChangesList")
                            .track_focus(&self.list_focus)
                            .on_action(cx.listener(|this, _: &SelectNextFile, _, cx| {
                                this.move_selection(1, cx)
                            }))
                            .on_action(cx.listener(|this, _: &SelectPreviousFile, _, cx| {
                                this.move_selection(-1, cx)
                            }))
                            .on_action(cx.listener(|this, _: &ToggleSelectedFile, _, cx| {
                                this.toggle_selected(cx)
                            }))
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(list_body),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(px(10.))
                            .pt(px(12.))
                            .pb(px(14.))
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .border_t_1()
                            .border_color(theme.border)
                            .child(Input::new(&self.summary).aria_label("Commit summary"))
                            .child(
                                Textarea::new(&self.description)
                                    .aria_label("Commit description")
                                    .h(px(80.)),
                            )
                            .when(has_commits, |box_| {
                                box_.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(8.))
                                                .child({
                                                    let view = cx.entity();
                                                    checkbox(
                                                        "amend",
                                                        if amending { Inclusion::All } else { Inclusion::None },
                                                        &theme,
                                                        move |window, cx| {
                                                            view.update(cx, |this, cx| this.toggle_amend(window, cx))
                                                        },
                                                    )
                                                })
                                                .child(div().text_color(theme.text_muted).child("Amend last commit")),
                                        )
                                        .when(amending && already_pushed, |column| {
                                            column.child(
                                                div()
                                                    .text_size(px(theme.metrics.text_sm))
                                                    .text_color(theme.modified.text)
                                                    .child("This commit is already pushed. Amending it means a force push."),
                                            )
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .id("commit")
                                    .h(px(theme.metrics.control_height))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(theme.metrics.radius_md))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .truncate()
                                    .px(px(12.))
                                    .when(can_commit, |button| {
                                        button
                                            .bg(theme.accent)
                                            .text_color(theme.accent_text)
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| this.commit(cx)))
                                    })
                                    .when(!can_commit, |button| {
                                        button.bg(theme.pill_active).text_color(theme.text_muted)
                                    })
                                    .child(commit_label),
                            ),
                    ),
            )
            .child(panel(&theme).flex_1().min_w_0().child(detail))
    }
}

#[cfg(test)]
mod tests {
    use super::{SyncAction, SyncKind, clean_tree_next_step, clean_tree_summary, split_path};

    #[test]
    fn the_clean_tree_suggests_the_sync_that_is_waiting() {
        let action = |kind, count: Option<u32>| SyncAction {
            kind,
            label: String::new(),
            count,
            remote: "origin".into(),
        };
        let step = |a: SyncAction| clean_tree_next_step(&a).map(|(_, text)| text);
        assert_eq!(
            step(action(SyncKind::Push, Some(2))).as_deref(),
            Some("Push 2 commits to origin")
        );
        assert_eq!(
            step(action(SyncKind::Push, Some(1))).as_deref(),
            Some("Push 1 commit to origin")
        );
        assert_eq!(
            step(action(SyncKind::Pull, Some(3))).as_deref(),
            Some("Pull 3 commits from origin")
        );
        assert_eq!(
            step(action(SyncKind::Publish, None)).as_deref(),
            Some("Publish this branch to origin")
        );
        assert_eq!(step(action(SyncKind::Fetch, None)), None);
    }

    #[test]
    fn the_clean_tree_summary_agrees_with_the_number_of_commits() {
        assert_eq!(
            clean_tree_summary(0),
            "Your working tree matches the last commit."
        );
        assert!(clean_tree_summary(1).ends_with("1 commit is waiting to be pushed."));
        assert!(clean_tree_summary(2).ends_with("2 commits are waiting to be pushed."));
    }

    #[test]
    fn splits_a_nested_path_into_name_and_directory() {
        assert_eq!(
            split_path("src/ui/diff_view.rs"),
            ("diff_view.rs", "src/ui")
        );
    }

    #[test]
    fn a_top_level_file_has_no_directory() {
        assert_eq!(split_path("Cargo.toml"), ("Cargo.toml", ""));
    }
}
