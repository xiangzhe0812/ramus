use git_core::{ChangeKind, FileChange};
use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
    uniform_list,
};

use crate::{
    repo_state::RepoState,
    theme::{Badge, MONO_FONT, Theme},
};

/// The Changes tab: the changed-files list and the pane for the selected file.
pub struct ChangesView {
    repo_state: Entity<RepoState>,
    selected: Option<String>,
    _observe: Subscription,
}

impl ChangesView {
    pub fn new(repo_state: Entity<RepoState>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&repo_state, |_, _, cx| cx.notify());
        Self {
            repo_state,
            selected: None,
            _observe,
        }
    }
}

/// Splits `src/ui/diff_view.rs` into (`diff_view.rs`, `src/ui`).
fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

fn badge_for(kind: ChangeKind, theme: &Theme) -> (&'static str, &'static str, Badge) {
    match kind {
        ChangeKind::Modified | ChangeKind::TypeChanged => ("M", "Modified", theme.modified),
        ChangeKind::Added | ChangeKind::Untracked => ("A", "Added", theme.added),
        ChangeKind::Deleted => ("D", "Deleted", theme.deleted),
        ChangeKind::Renamed | ChangeKind::Copied => ("R", "Renamed", theme.renamed),
        ChangeKind::Conflicted => ("!", "Conflicted", theme.conflicted),
    }
}

fn file_row(
    ix: usize,
    file: &FileChange,
    selected: bool,
    theme: &Theme,
    on_select: impl Fn(&mut Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    let (name, dir) = split_path(&file.path);
    let (letter, _, badge) = badge_for(file.kind(), theme);
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
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_muted)
                .child("No changed files")
                .into_any_element()
        } else {
            let row_theme = theme.clone();
            uniform_list(
                "changed-files",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .filter_map(|ix| {
                            let file = files.get(ix)?;
                            let selected = this.selected.as_deref() == Some(file.path.as_str());
                            let path = file.path.clone();
                            let view = cx.entity();
                            Some(file_row(ix, file, selected, &row_theme, move |_, cx| {
                                view.update(cx, |this, cx| {
                                    this.selected = Some(path.clone());
                                    cx.notify();
                                })
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
                }))
                .into_any_element()
        } else {
            div()
                .text_color(theme.text_muted)
                .child("Select a file to see its changes")
                .into_any_element()
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
            .child(
                panel(&theme)
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_center()
                    .child(detail),
            )
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
