//! Drawing a diff: the rows and the scrolling list that holds them. Shared by the
//! Changes tab and the History tab.

use std::{collections::HashSet, ops::Range, rc::Rc};

use git_core::{DiffLineKind, FileDiffs};
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ListHorizontalSizingBehavior,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder,
    px, uniform_list,
};

use crate::{
    diff_rows::{DiffRow, HunkAction, HunkControls, LineRef, flatten, longest_row},
    theme::{MONO_FONT, Theme, UI_FONT},
};

const DIFF_ROW_HEIGHT: f32 = 20.;
const LINE_NUMBER_WIDTH: f32 = 52.;

/// What a hunk's button does when clicked.
pub type HunkHandler = Rc<dyn Fn(HunkAction, &mut App)>;

/// What picking a changed line does.
pub type LineHandler = Rc<dyn Fn(LineRef, &mut App)>;

/// How a diff reacts to the user: its hunk buttons, picking lines, and which lines are
/// picked now.
#[derive(Clone)]
pub struct DiffInteraction {
    pub on_hunk: HunkHandler,
    pub on_line: LineHandler,
    pub selection: Rc<HashSet<LineRef>>,
}

impl DiffInteraction {
    /// For a diff that can't be changed, such as a commit in the history.
    pub fn read_only() -> Self {
        Self {
            on_hunk: Rc::new(|_, _| {}),
            on_line: Rc::new(|_, _| {}),
            selection: Rc::default(),
        }
    }
}

fn diff_row(ix: usize, row: &DiffRow, theme: &Theme, interaction: &DiffInteraction) -> AnyElement {
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
        DiffRow::Section(title) => base
            .bg(theme.pill_active)
            .text_color(theme.text)
            .font_family(UI_FONT)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_size(px(11.5))
            .pl(px(14.))
            .child(title.clone())
            .into_any_element(),
        DiffRow::Hunk { header, action } => {
            let button = action.map(|action| {
                let on_hunk = interaction.on_hunk.clone();
                let picked = interaction
                    .selection
                    .iter()
                    .filter(|line| line.staged == action.staged && line.hunk == action.index)
                    .count();
                let verb = if action.staged { "Unstage" } else { "Stage" };
                div()
                    .id(("hunk-action", ix))
                    .h_full()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .font_family(UI_FONT)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_size(px(11.5))
                    .text_color(theme.accent_soft)
                    .hover(|button| button.bg(theme.surface_selected))
                    .child(match picked {
                        0 => format!("{verb} hunk"),
                        1 => format!("{verb} 1 line"),
                        n => format!("{verb} {n} lines"),
                    })
                    .on_click(move |_, _, cx| on_hunk(action, cx))
            });
            base.bg(theme.diff_hunk_background)
                .text_color(theme.diff_hunk_text)
                .pl(px(LINE_NUMBER_WIDTH * 2.))
                .child(div().flex_1().min_w_0().truncate().child(header.clone()))
                .children(button)
                .into_any_element()
        }
        DiffRow::Line {
            kind,
            old_number,
            new_number,
            text,
            line_ref,
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
                .child({
                    // The +/- marker doubles as the way to pick the line.
                    let picked = line_ref.is_some_and(|line| interaction.selection.contains(&line));
                    let cell = div()
                        .id(("pick", ix))
                        .w(px(20.))
                        .h(px(16.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .text_color(if picked {
                            theme.accent_text
                        } else {
                            sign_color
                        })
                        .when(picked, |cell| cell.bg(theme.accent));
                    match line_ref {
                        Some(line) => {
                            let on_line = interaction.on_line.clone();
                            let line = *line;
                            cell.cursor_pointer()
                                .hover(|cell| cell.bg(theme.pill_active))
                                .on_click(move |_, _, cx| on_line(line, cx))
                                .child(sign)
                        }
                        None => cell.child(sign),
                    }
                })
                .child(
                    // Natural width, so the longest line decides how far the list scrolls.
                    div()
                        .flex_none()
                        .pr(px(24.))
                        .whitespace_nowrap()
                        .text_color(text_color)
                        .child(text.clone()),
                )
                .into_any_element()
        }
    }
}

/// A scrolling list of diff rows that can also scroll sideways to show long lines.
/// `longest` is the row with the most text, which decides how wide the list is.
pub fn diff_list(
    id: impl Into<ElementId>,
    rows: Rc<Vec<DiffRow>>,
    longest: Option<usize>,
    theme: &Theme,
    interaction: DiffInteraction,
) -> AnyElement {
    let theme = theme.clone();
    uniform_list(id, rows.len(), {
        let rows = rows.clone();
        move |range: Range<usize>, _window, _cx| {
            range
                .filter_map(|ix| Some(diff_row(ix, rows.get(ix)?, &theme, &interaction)))
                .collect::<Vec<_>>()
        }
    })
    .with_width_from_item(longest)
    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    .flex_1()
    .py(px(8.))
    .into_any_element()
}

/// What a diff pane has to show.
pub enum DiffContent {
    Rows {
        rows: Rc<Vec<DiffRow>>,
        /// The row with the longest line, which sizes the list so it can scroll sideways.
        longest: Option<usize>,
        additions: usize,
        deletions: usize,
    },
    Binary,
    /// Nothing to show as text, e.g. a mode change or an emptied file.
    Empty,
    Failed(SharedString),
}

impl DiffContent {
    pub fn from_diffs(diffs: &FileDiffs, controls: HunkControls) -> Self {
        if diffs.binary() {
            Self::Binary
        } else if diffs.is_empty() {
            Self::Empty
        } else {
            let rows = flatten(diffs, controls);
            Self::Rows {
                longest: longest_row(&rows),
                rows: Rc::new(rows),
                additions: diffs.additions(),
                deletions: diffs.deletions(),
            }
        }
    }
}

/// A centred line of muted text, for the states that have nothing to draw.
pub fn message(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
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

/// The body of a diff pane in whatever state it is in: loading, failed, nothing to show,
/// or the diff itself.
pub fn diff_body(
    content: Option<&DiffContent>,
    id: impl Into<ElementId>,
    hide_whitespace: bool,
    theme: &Theme,
    interaction: DiffInteraction,
) -> AnyElement {
    match content {
        None => message(theme, "Loading diff…"),
        Some(DiffContent::Binary) => message(theme, "Binary file not shown"),
        Some(DiffContent::Empty) => message(
            theme,
            if hide_whitespace {
                "No changes other than whitespace"
            } else {
                "No textual changes"
            },
        ),
        Some(DiffContent::Failed(text)) => message(theme, text.clone()),
        Some(DiffContent::Rows { rows, longest, .. }) => {
            diff_list(id, rows.clone(), *longest, theme, interaction)
        }
    }
}
