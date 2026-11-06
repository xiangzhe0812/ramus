use git_core::{DiffLineKind, FileDiff, FileDiffs};
use gpui::SharedString;

/// One changed line of a hunk, which the user can pick to stage on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineRef {
    /// The line is in the staged diff, so picking it is for unstaging.
    pub staged: bool,
    /// Index into that diff's hunks.
    pub hunk: usize,
    /// Index into that hunk's lines.
    pub line: usize,
}

/// What a hunk's button does, and which hunk it acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HunkAction {
    /// The hunk is in the staged diff, so the action unstages it.
    pub staged: bool,
    /// Index into that diff's hunks.
    pub index: usize,
}

/// One display row of a diff. Flattened up front so a virtualized list can
/// render any range of rows without walking hunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    /// Divides a file's unstaged changes from its staged ones.
    Section(SharedString),
    Hunk {
        header: SharedString,
        /// `None` when the file can't be staged hunk by hunk.
        action: Option<HunkAction>,
    },
    Line {
        kind: DiffLineKind,
        old_number: Option<u32>,
        new_number: Option<u32>,
        text: SharedString,
        /// Set for changed lines of a file that can be staged piece by piece.
        line_ref: Option<LineRef>,
    },
}

/// Display text for a source line: tabs become spaces so columns line up, and a
/// carriage return from CRLF files is dropped rather than drawn.
fn display_text(text: &str) -> SharedString {
    text.replace('\t', "    ").replace('\r', "").into()
}

/// Which hunks of a file get a stage or unstage button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HunkControls {
    pub stage: bool,
    pub unstage: bool,
}

fn push_section(rows: &mut Vec<DiffRow>, diff: &FileDiff, staged: bool, enabled: bool) {
    for (index, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DiffRow::Hunk {
            header: hunk.header().into(),
            action: enabled.then_some(HunkAction { staged, index }),
        });
        rows.extend(
            hunk.lines
                .iter()
                .enumerate()
                .map(|(line_ix, line)| DiffRow::Line {
                    kind: line.kind,
                    old_number: line.old_number,
                    new_number: line.new_number,
                    text: display_text(&line.text),
                    line_ref: (enabled && line.kind != DiffLineKind::Context).then_some(LineRef {
                        staged,
                        hunk: index,
                        line: line_ix,
                    }),
                }),
        );
    }
}

/// The index of the row with the most text, so a list can be sized to fit it and scroll
/// sideways to reveal long lines. Hunk headers and section titles don't count.
pub fn longest_row(rows: &[DiffRow]) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| match row {
            DiffRow::Line { text, .. } => Some((ix, text.chars().count())),
            _ => None,
        })
        .max_by_key(|(_, width)| *width)
        .map(|(ix, _)| ix)
}

/// Unstaged changes first, then staged ones. The two sections are labelled only when
/// both exist, since a file with just one kind of change needs no explanation.
pub fn flatten(diffs: &FileDiffs, controls: HunkControls) -> Vec<DiffRow> {
    let both = !diffs.unstaged.hunks.is_empty() && !diffs.staged.hunks.is_empty();
    let mut rows = Vec::new();

    if both {
        rows.push(DiffRow::Section("Unstaged changes".into()));
    }
    push_section(&mut rows, &diffs.unstaged, false, controls.stage);
    if both {
        rows.push(DiffRow::Section("Staged changes".into()));
    }
    push_section(&mut rows, &diffs.staged, true, controls.unstage);
    rows
}

#[cfg(test)]
mod tests {
    use git_core::{DiffHunk, DiffLine};

    use super::*;

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_number: Some(1),
            new_number: Some(1),
            text: text.into(),
            no_newline_at_eof: false,
        }
    }

    fn diff(hunk_count: usize) -> FileDiff {
        FileDiff {
            hunks: (0..hunk_count)
                .map(|_| DiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    section: String::new(),
                    lines: vec![
                        line(DiffLineKind::Added, "a"),
                        line(DiffLineKind::Removed, "b"),
                    ],
                })
                .collect(),
            binary: false,
        }
    }

    fn on() -> HunkControls {
        HunkControls {
            stage: true,
            unstage: true,
        }
    }

    #[test]
    fn each_hunk_contributes_a_header_row_then_its_lines() {
        let diffs = FileDiffs {
            staged: FileDiff::default(),
            unstaged: diff(2),
        };
        let rows = flatten(&diffs, on());
        assert_eq!(rows.len(), 6);
        assert!(matches!(rows[0], DiffRow::Hunk { .. }));
        assert!(matches!(rows[3], DiffRow::Hunk { .. }));
    }

    #[test]
    fn a_file_with_one_kind_of_change_has_no_section_labels() {
        let diffs = FileDiffs {
            staged: FileDiff::default(),
            unstaged: diff(1),
        };
        assert!(
            !flatten(&diffs, on())
                .iter()
                .any(|row| matches!(row, DiffRow::Section(_)))
        );
    }

    #[test]
    fn a_file_with_both_kinds_is_labelled_unstaged_first_then_staged() {
        let diffs = FileDiffs {
            staged: diff(1),
            unstaged: diff(1),
        };
        let sections: Vec<_> = flatten(&diffs, on())
            .into_iter()
            .filter_map(|row| match row {
                DiffRow::Section(title) => Some(title.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(sections, ["Unstaged changes", "Staged changes"]);
    }

    #[test]
    fn hunk_actions_point_at_the_right_diff_and_index() {
        let diffs = FileDiffs {
            staged: diff(1),
            unstaged: diff(2),
        };
        let actions: Vec<_> = flatten(&diffs, on())
            .into_iter()
            .filter_map(|row| match row {
                DiffRow::Hunk { action, .. } => action,
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            [
                HunkAction {
                    staged: false,
                    index: 0
                },
                HunkAction {
                    staged: false,
                    index: 1
                },
                HunkAction {
                    staged: true,
                    index: 0
                },
            ]
        );
    }

    #[test]
    fn only_changed_lines_of_a_stageable_file_can_be_picked() {
        let mut diffs = FileDiffs {
            staged: FileDiff::default(),
            unstaged: diff(1),
        };
        diffs.unstaged.hunks[0]
            .lines
            .insert(0, line(DiffLineKind::Context, "ctx"));
        let picks = |controls| -> Vec<Option<LineRef>> {
            flatten(&diffs, controls)
                .into_iter()
                .filter_map(|row| match row {
                    DiffRow::Line { line_ref, .. } => Some(line_ref),
                    _ => None,
                })
                .collect()
        };

        let on = picks(on());
        assert_eq!(on[0], None, "context lines can't be picked");
        assert_eq!(
            on[1],
            Some(LineRef {
                staged: false,
                hunk: 0,
                line: 1
            })
        );
        assert_eq!(
            on[2],
            Some(LineRef {
                staged: false,
                hunk: 0,
                line: 2
            })
        );
        assert!(picks(HunkControls::default()).iter().all(Option::is_none));
    }

    #[test]
    fn controls_can_be_withheld_per_side() {
        let diffs = FileDiffs {
            staged: diff(1),
            unstaged: diff(1),
        };
        let rows = flatten(
            &diffs,
            HunkControls {
                stage: true,
                unstage: false,
            },
        );
        let actions: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                DiffRow::Hunk { action, .. } => Some(action.is_some()),
                _ => None,
            })
            .collect();
        assert_eq!(actions, [true, false]);
    }

    #[test]
    fn the_longest_row_is_the_line_with_the_most_text() {
        let mut diffs = FileDiffs {
            staged: FileDiff::default(),
            unstaged: diff(1),
        };
        diffs.unstaged.hunks[0].lines.push(line(
            DiffLineKind::Context,
            "a much longer line than the rest",
        ));
        let rows = flatten(&diffs, on());
        let longest = longest_row(&rows).unwrap();
        assert!(
            matches!(&rows[longest], DiffRow::Line { text, .. } if text.starts_with("a much longer"))
        );
    }

    #[test]
    fn there_is_no_longest_row_in_a_diff_without_lines() {
        assert_eq!(longest_row(&[DiffRow::Section("x".into())]), None);
        assert_eq!(longest_row(&[]), None);
    }

    #[test]
    fn tabs_and_carriage_returns_are_normalised_for_display() {
        assert_eq!(display_text("\tfn x() {}\r").as_ref(), "    fn x() {}");
    }
}
