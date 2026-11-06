use std::{ops::Range, rc::Rc};

use git_core::{ChangeKind, DiffLineKind, DiffOptions, FileChange, FileDiff};
use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder, px, uniform_list,
};

use crate::{
    diff_rows::{DiffRow, flatten},
    repo_state::RepoState,
    theme::{Badge, MONO_FONT, Theme},
};

/// What the diff pane has for the selected file.
enum DiffContent {
    Rows {
        rows: Rc<Vec<DiffRow>>,
        additions: usize,
        deletions: usize,
    },
    Binary,
    /// Nothing to show as text, e.g. a mode change or an emptied file.
    Empty,
    Failed(SharedString),
}

struct LoadedDiff {
    /// The raw diff, kept to skip a redraw when a refresh finds nothing new.
    source: Option<FileDiff>,
    content: DiffContent,
}

impl LoadedDiff {
    fn from_result(result: Result<FileDiff, git_core::Error>) -> Self {
        match result {
            Ok(diff) if diff.binary => Self {
                content: DiffContent::Binary,
                source: Some(diff),
            },
            Ok(diff) if diff.hunks.is_empty() => Self {
                content: DiffContent::Empty,
                source: Some(diff),
            },
            Ok(diff) => Self {
                content: DiffContent::Rows {
                    rows: Rc::new(flatten(&diff)),
                    additions: diff.additions(),
                    deletions: diff.deletions(),
                },
                source: Some(diff),
            },
            Err(error) => Self {
                content: DiffContent::Failed(error.to_string().into()),
                source: None,
            },
        }
    }
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
    _observe: Subscription,
}

impl ChangesView {
    pub fn new(repo_state: Entity<RepoState>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&repo_state, |this, _, cx| this.repo_changed(cx));
        Self {
            repo_state,
            selected: None,
            diff: None,
            hide_whitespace: false,
            diff_generation: 0,
            diff_task: None,
            _observe,
        }
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(path.as_str()) {
            return;
        }
        self.selected = Some(path);
        self.diff = None;
        self.diff_generation += 1;
        self.load_diff(cx);
        cx.notify();
    }

    fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        self.hide_whitespace = !self.hide_whitespace;
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
            self.diff = Some(LoadedDiff {
                source: None,
                content: DiffContent::Failed("Merge conflicts can't be viewed yet.".into()),
            });
            return;
        }

        let options = DiffOptions {
            ignore_whitespace: self.hide_whitespace,
        };
        let generation = self.diff_generation;
        self.diff_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.diff(&change, options) })
                .await;

            this.update(cx, |this, cx| {
                if this.diff_generation != generation {
                    return;
                }
                // A refresh that finds the same diff must not redraw or reset the scroll.
                let unchanged = matches!(
                    (&result, this.diff.as_ref().and_then(|d| d.source.as_ref())),
                    (Ok(new), Some(old)) if new == old
                );
                if !unchanged {
                    this.diff = Some(LoadedDiff::from_result(result));
                    cx.notify();
                }
            })
            .ok();
        }));
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

fn file_row(
    ix: usize,
    file: &FileChange,
    selected: bool,
    theme: &Theme,
    on_select: impl Fn(&mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let (name, dir) = split_path(&file.path);
    let (letter, badge) = badge_for(file.kind(), theme);
    let staged = file.is_staged();

    div()
        .id(("file", ix))
        .w_full()
        .h(px(44.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.surface_selected))
        .hover(|row| row.bg(theme.surface_selected))
        .on_click(move |_, window, cx| on_select(window, cx))
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .rounded(px(4.))
                .border_1()
                .border_color(if staged {
                    theme.accent
                } else {
                    theme.text_muted
                })
                .when(staged, |checkbox| checkbox.bg(theme.accent))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(theme.accent_text)
                .when(staged, |checkbox| checkbox.child("✓")),
        )
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
                            .text_size(px(11.))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(dir.to_owned())),
                    )
                }),
        )
        .child(
            div()
                .size(px(20.))
                .flex_none()
                .rounded(px(10.))
                .bg(badge.background)
                .text_color(badge.text)
                .font_family(MONO_FONT)
                .text_size(px(11.))
                .flex()
                .items_center()
                .justify_center()
                .child(letter),
        )
        .into_any_element()
}

const DIFF_ROW_HEIGHT: f32 = 20.;
const LINE_NUMBER_WIDTH: f32 = 52.;

fn diff_row(ix: usize, row: &DiffRow, theme: &Theme) -> AnyElement {
    let number = |value: Option<u32>| {
        div()
            .w(px(LINE_NUMBER_WIDTH))
            .flex_none()
            .pr(px(10.))
            .text_right()
            .text_color(theme.diff_line_number)
            .child(value.map(|n| n.to_string()).unwrap_or_default())
    };

    let base = div()
        .id(("diff-row", ix))
        .w_full()
        .h(px(DIFF_ROW_HEIGHT))
        .flex()
        .items_center()
        .font_family(MONO_FONT)
        .text_size(px(12.5));

    match row {
        DiffRow::Hunk(header) => base
            .bg(theme.diff_hunk_background)
            .text_color(theme.diff_hunk_text)
            .pl(px(LINE_NUMBER_WIDTH * 2.))
            .child(header.clone())
            .into_any_element(),
        DiffRow::Line {
            kind,
            old_number,
            new_number,
            text,
        } => {
            let (background, sign, sign_color, text_color) = match kind {
                DiffLineKind::Added => (
                    Some(theme.diff_added_background),
                    "+",
                    theme.diff_added_sign,
                    theme.diff_added_text,
                ),
                DiffLineKind::Removed => (
                    Some(theme.diff_removed_background),
                    "−",
                    theme.diff_removed_sign,
                    theme.diff_removed_text,
                ),
                DiffLineKind::Context => (None, " ", theme.text, theme.diff_context_text),
            };
            base.when_some(background, |row, color| row.bg(color))
                .child(number(*old_number))
                .child(number(*new_number))
                .child(
                    div()
                        .w(px(20.))
                        .flex_none()
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_color(text_color)
                        .child(text.clone()),
                )
                .into_any_element()
        }
    }
}

impl ChangesView {
    fn pill(text: String, colors: Badge) -> impl IntoElement {
        div()
            .px(px(8.))
            .rounded(px(10.))
            .bg(colors.background)
            .text_color(colors.text)
            .font_family(MONO_FONT)
            .text_size(px(11.5))
            .child(text)
    }

    fn message(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .px(px(24.))
            .text_color(theme.text_muted)
            .child(text.into())
            .into_any_element()
    }

    fn diff_pane(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(path) = self.selected.clone() else {
            return Self::message(theme, "Select a file to see its changes");
        };

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
            .h(px(48.))
            .flex_none()
            .px(px(16.))
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
                    .text_size(px(12.5))
                    .child(SharedString::from(path)),
            )
            .children(additions.map(|n| Self::pill(format!("+{n}"), theme.added)))
            .children(deletions.map(|n| Self::pill(format!("−{n}"), theme.deleted)))
            .child(div().flex_1())
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
                            .rounded(px(4.))
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
                            .text_size(px(11.))
                            .text_color(theme.accent_text)
                            .when(hide_whitespace, |checkbox| checkbox.child("✓")),
                    )
                    .child("Hide whitespace"),
            );

        let body = match self.diff.as_ref().map(|diff| &diff.content) {
            None => Self::message(theme, "Loading diff…"),
            Some(DiffContent::Binary) => Self::message(theme, "Binary file not shown"),
            Some(DiffContent::Empty) => Self::message(
                theme,
                if hide_whitespace {
                    "No changes other than whitespace"
                } else {
                    "No textual changes"
                },
            ),
            Some(DiffContent::Failed(message)) => Self::message(theme, message.clone()),
            Some(DiffContent::Rows { rows, .. }) => {
                let rows = rows.clone();
                let theme = theme.clone();
                uniform_list(
                    ("diff-rows", self.diff_generation as usize),
                    rows.len(),
                    cx.processor(move |_, range: Range<usize>, _window, _cx| {
                        range
                            .filter_map(|ix| Some(diff_row(ix, rows.get(ix)?, &theme)))
                            .collect()
                    }),
                )
                .flex_1()
                .py(px(8.))
                .into_any_element()
            }
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
        let ahead = self
            .repo_state
            .read(cx)
            .status()
            .map_or(0, |status| status.branch.ahead);

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

        let list_body = if count == 0 {
            Self::message(&theme, "No changed files")
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
                            Some(file_row(ix, file, selected, &row_theme, move |_, cx| {
                                view.update(cx, |this, cx| this.select(path.clone(), cx))
                            }))
                        })
                        .collect()
                }),
            )
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
                                .text_size(px(20.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("No local changes"),
                        )
                        .child(div().text_color(theme.text_muted).child(if ahead > 0 {
                            format!("Your working tree matches the last commit. {ahead} commits are waiting to be pushed.")
                        } else {
                            "Your working tree matches the last commit.".to_owned()
                        })),
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
            .px(px(16.))
            .pb(px(12.))
            .child(
                panel(&theme)
                    .w(px(330.))
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
                                    .child("Changes"),
                            )
                            .child(
                                div()
                                    .px(px(9.))
                                    .rounded(px(10.))
                                    .bg(theme.pill_active)
                                    .text_size(px(11.5))
                                    .text_color(theme.text_muted)
                                    .child(if count == 1 {
                                        "1 file".to_owned()
                                    } else {
                                        format!("{count} files")
                                    }),
                            ),
                    )
                    .child(list_body),
            )
            .child(panel(&theme).flex_1().min_w_0().child(detail))
    }
}

#[cfg(test)]
mod tests {
    use super::split_path;

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
