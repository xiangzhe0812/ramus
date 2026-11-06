use git_core::{DiffLineKind, FileDiff};
use gpui::SharedString;

/// One display row of a diff. Flattened up front so a virtualized list can
/// render any range of rows without walking hunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Hunk(SharedString),
    Line {
        kind: DiffLineKind,
        old_number: Option<u32>,
        new_number: Option<u32>,
        text: SharedString,
    },
}

/// Display text for a source line: tabs become spaces so columns line up, and a
/// carriage return from CRLF files is dropped rather than drawn.
fn display_text(text: &str) -> SharedString {
    text.replace('\t', "    ").replace('\r', "").into()
}

pub fn flatten(diff: &FileDiff) -> Vec<DiffRow> {
    let mut rows = Vec::with_capacity(diff.hunks.iter().map(|hunk| hunk.lines.len() + 1).sum());
    for hunk in &diff.hunks {
        rows.push(DiffRow::Hunk(hunk.header().into()));
        rows.extend(hunk.lines.iter().map(|line| DiffRow::Line {
            kind: line.kind,
            old_number: line.old_number,
            new_number: line.new_number,
            text: display_text(&line.text),
        }));
    }
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

    #[test]
    fn each_hunk_contributes_a_header_row_then_its_lines() {
        let hunk = |lines| DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            section: String::new(),
            lines,
        };
        let diff = FileDiff {
            hunks: vec![
                hunk(vec![
                    line(DiffLineKind::Added, "a"),
                    line(DiffLineKind::Removed, "b"),
                ]),
                hunk(vec![line(DiffLineKind::Context, "c")]),
            ],
            binary: false,
        };

        let rows = flatten(&diff);
        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[0], DiffRow::Hunk(_)));
        assert!(matches!(rows[3], DiffRow::Hunk(_)));
    }

    #[test]
    fn tabs_and_carriage_returns_are_normalised_for_display() {
        assert_eq!(display_text("\tfn x() {}\r").as_ref(), "    fn x() {}");
    }
}
