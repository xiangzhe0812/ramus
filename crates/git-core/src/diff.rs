use std::collections::BTreeSet;

use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Line number in the old file; `None` for added lines.
    pub old_number: Option<u32>,
    /// Line number in the new file; `None` for removed lines.
    pub new_number: Option<u32>,
    pub text: String,
    /// The file has no newline after this line (git's `\ No newline at end of file`).
    pub no_newline_at_eof: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The text after the second `@@`, usually the enclosing function.
    pub section: String,
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    /// The header as git prints it, e.g. `@@ -18,7 +18,14 @@ impl Render for DiffView`.
    pub fn header(&self) -> String {
        let mut header = format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        );
        if !self.section.is_empty() {
            header.push(' ');
            header.push_str(&self.section);
        }
        header
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub hunks: Vec<DiffHunk>,
    pub binary: bool,
}

impl FileDiff {
    fn count(&self, kind: DiffLineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == kind)
            .count()
    }

    pub fn additions(&self) -> usize {
        self.count(DiffLineKind::Added)
    }

    pub fn deletions(&self) -> usize {
        self.count(DiffLineKind::Removed)
    }
}

/// A file's changes split by where they are: `staged` is HEAD → index (what the next
/// commit will contain) and `unstaged` is index → working tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiffs {
    pub staged: FileDiff,
    pub unstaged: FileDiff,
}

impl FileDiffs {
    pub fn binary(&self) -> bool {
        self.staged.binary || self.unstaged.binary
    }

    pub fn additions(&self) -> usize {
        self.staged.additions() + self.unstaged.additions()
    }

    pub fn deletions(&self) -> usize {
        self.staged.deletions() + self.unstaged.deletions()
    }

    pub fn is_empty(&self) -> bool {
        self.staged.hunks.is_empty() && self.unstaged.hunks.is_empty()
    }
}

/// `prefix/path`, C-quoted the way git does when the name has characters that would
/// confuse a patch header (quotes, backslashes, control characters).
fn patch_path(prefix: &str, path: &str) -> String {
    let full = format!("{prefix}/{path}");
    if !full
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\')
    {
        return full;
    }
    let mut quoted = String::from("\"");
    for ch in full.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            '\r' => quoted.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                quoted.push_str(&format!("\\{:03o}", c as u32))
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// A patch for `path` containing only `hunks`, for `git apply`. Each hunk keeps its own
/// header, so applying a subset of whole hunks needs no recounting.
pub(crate) fn patch_for(path: &str, hunks: &[&DiffHunk]) -> String {
    let old = patch_path("a", path);
    let new = patch_path("b", path);
    // Git adds a tab after a name containing a space so the name's end is unambiguous.
    let tab = |name: &str| if name.contains(' ') { "\t" } else { "" };

    let mut patch = format!(
        "diff --git {old} {new}\n--- {old}{}\n+++ {new}{}\n",
        tab(&old),
        tab(&new)
    );
    for hunk in hunks {
        patch.push_str(&hunk.header());
        patch.push('\n');
        for line in &hunk.lines {
            patch.push(match line.kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Added => '+',
                DiffLineKind::Removed => '-',
            });
            patch.push_str(&line.text);
            patch.push('\n');
            if line.no_newline_at_eof {
                patch.push_str("\\ No newline at end of file\n");
            }
        }
    }
    patch
}

/// Why a partial patch couldn't be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinePatchError {
    /// None of the selected lines is a change.
    NothingSelected,
    /// The selection would leave a line without its final newline in the middle of the
    /// file, which a patch can't express.
    MissingNewlineAtEnd,
}

/// A patch that applies only the changed lines of `hunk` whose indexes are in `selected`
/// (indexes into `hunk.lines`).
///
/// A hunk can't just have lines deleted from it: the lines left out have to be accounted
/// for so the patch still fits the file. Staging (applying forward to the index):
/// an unselected added line is simply not added, and an unselected removed line is kept,
/// so it becomes context. Unstaging (applying the staged diff in reverse): an unselected
/// added line stays in the index, so it becomes context, and an unselected removed line
/// stays removed, so it is left out. The counts in the header are recomputed here.
pub(crate) fn patch_for_lines(
    path: &str,
    hunk: &DiffHunk,
    selected: &BTreeSet<usize>,
    reverse: bool,
) -> Result<String, LinePatchError> {
    let mut lines: Vec<(char, &DiffLine)> = Vec::new();
    let mut changes = 0;
    for (ix, line) in hunk.lines.iter().enumerate() {
        let chosen = selected.contains(&ix);
        let prefix = match (line.kind, chosen) {
            (DiffLineKind::Context, _) => ' ',
            (DiffLineKind::Added, true) => '+',
            (DiffLineKind::Removed, true) => '-',
            // Not chosen: it either becomes context or disappears from the patch.
            (DiffLineKind::Added, false) if reverse => ' ',
            (DiffLineKind::Removed, false) if !reverse => ' ',
            (DiffLineKind::Added | DiffLineKind::Removed, false) => continue,
        };
        if prefix != ' ' {
            changes += 1;
        }
        lines.push((prefix, line));
    }
    if changes == 0 {
        return Err(LinePatchError::NothingSelected);
    }
    // A line with no final newline can only be the last line of its side of the file.
    for (ix, (prefix, line)) in lines.iter().enumerate() {
        if !line.no_newline_at_eof {
            continue;
        }
        let later = &lines[ix + 1..];
        let old_side_goes_on = *prefix != '+' && later.iter().any(|(p, _)| *p != '+');
        let new_side_goes_on = *prefix != '-' && later.iter().any(|(p, _)| *p != '-');
        if old_side_goes_on || new_side_goes_on {
            return Err(LinePatchError::MissingNewlineAtEnd);
        }
    }

    let old_lines = lines.iter().filter(|(p, _)| *p != '+').count() as u32;
    let new_lines = lines.iter().filter(|(p, _)| *p != '-').count() as u32;
    let rewritten = DiffHunk {
        old_start: hunk.old_start,
        old_lines,
        new_start: hunk.new_start,
        new_lines,
        section: hunk.section.clone(),
        lines: Vec::new(),
    };

    let mut patch = patch_for(path, &[&rewritten]);
    for (prefix, line) in lines {
        patch.push(prefix);
        patch.push_str(&line.text);
        patch.push('\n');
        if line.no_newline_at_eof {
            patch.push_str("\\ No newline at end of file\n");
        }
    }
    Ok(patch)
}

/// Parses `@@ -a[,b] +c[,d] @@ section`. A missing count means 1.
fn parse_hunk_header(line: &str) -> Result<DiffHunk, Error> {
    let bad = || Error::Parse(format!("bad hunk header {line:?}"));
    let rest = line.strip_prefix("@@ ").ok_or_else(bad)?;
    let (ranges, section) = rest.split_once(" @@").ok_or_else(bad)?;
    let (old, new) = ranges.split_once(' ').ok_or_else(bad)?;

    let range = |text: &str, sign: char| -> Result<(u32, u32), Error> {
        let text = text.strip_prefix(sign).ok_or_else(bad)?;
        let (start, count) = text.split_once(',').unwrap_or((text, "1"));
        Ok((
            start.parse().map_err(|_| bad())?,
            count.parse().map_err(|_| bad())?,
        ))
    };
    let (old_start, old_lines) = range(old, '-')?;
    let (new_start, new_lines) = range(new, '+')?;

    Ok(DiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section: section.trim().to_owned(),
        lines: Vec::new(),
    })
}

/// Parses the output of `git diff` for a single file.
pub(crate) fn parse_diff(output: &[u8]) -> Result<FileDiff, Error> {
    let text = String::from_utf8_lossy(output);
    let mut diff = FileDiff::default();
    let mut current: Option<(DiffHunk, u32, u32)> = None;

    let finish = |diff: &mut FileDiff, current: &mut Option<(DiffHunk, u32, u32)>| {
        if let Some((hunk, _, _)) = current.take() {
            diff.hunks.push(hunk);
        }
    };

    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }

    for line in lines {
        if line.starts_with("@@@") {
            return Err(Error::Parse("combined diffs are not supported".into()));
        }
        if line.starts_with("@@ ") {
            finish(&mut diff, &mut current);
            let hunk = parse_hunk_header(line)?;
            let (old, new) = (hunk.old_start, hunk.new_start);
            current = Some((hunk, old, new));
            continue;
        }

        let Some((hunk, old, new)) = current.as_mut() else {
            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                diff.binary = true;
            }
            continue;
        };

        if line.starts_with('\\') {
            if let Some(last) = hunk.lines.last_mut() {
                last.no_newline_at_eof = true;
            }
            continue;
        }

        let (kind, text) = match line.chars().next() {
            Some('+') => (DiffLineKind::Added, &line[1..]),
            Some('-') => (DiffLineKind::Removed, &line[1..]),
            Some(' ') => (DiffLineKind::Context, &line[1..]),
            // Some tools strip the single space from blank context lines.
            None => (DiffLineKind::Context, ""),
            Some(_) => {
                // Anything else ends this file's diff (e.g. the next `diff --git` header).
                finish(&mut diff, &mut current);
                continue;
            }
        };

        let (old_number, new_number) = match kind {
            DiffLineKind::Context => {
                let numbers = (Some(*old), Some(*new));
                *old += 1;
                *new += 1;
                numbers
            }
            DiffLineKind::Removed => {
                let numbers = (Some(*old), None);
                *old += 1;
                numbers
            }
            DiffLineKind::Added => {
                let numbers = (None, Some(*new));
                *new += 1;
                numbers
            }
        };
        hunk.lines.push(DiffLine {
            kind,
            old_number,
            new_number,
            text: text.to_owned(),
            no_newline_at_eof: false,
        });
    }
    finish(&mut diff, &mut current);

    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "diff --git a/f.rs b/f.rs\nindex 111..222 100644\n--- a/f.rs\n+++ b/f.rs\n";

    fn parse(body: &str) -> FileDiff {
        parse_diff(format!("{HEADER}{body}").as_bytes()).unwrap()
    }

    #[test]
    fn parses_a_hunk_with_line_numbers() {
        let diff = parse(
            "@@ -18,3 +18,4 @@ impl Render for DiffView\n \
             keep\n\
             -old\n\
             +new one\n\
             +new two\n \
             tail\n",
        );

        assert_eq!(diff.hunks.len(), 1);
        let hunk = &diff.hunks[0];
        assert_eq!(
            (
                hunk.old_start,
                hunk.old_lines,
                hunk.new_start,
                hunk.new_lines
            ),
            (18, 3, 18, 4)
        );
        assert_eq!(hunk.section, "impl Render for DiffView");
        assert_eq!(hunk.header(), "@@ -18,3 +18,4 @@ impl Render for DiffView");

        let summary: Vec<_> = hunk
            .lines
            .iter()
            .map(|l| (l.kind, l.old_number, l.new_number, l.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (DiffLineKind::Context, Some(18), Some(18), "keep"),
                (DiffLineKind::Removed, Some(19), None, "old"),
                (DiffLineKind::Added, None, Some(19), "new one"),
                (DiffLineKind::Added, None, Some(20), "new two"),
                (DiffLineKind::Context, Some(20), Some(21), "tail"),
            ]
        );
        assert_eq!((diff.additions(), diff.deletions()), (2, 1));
    }

    #[test]
    fn a_missing_count_means_one_line() {
        let diff = parse("@@ -1 +1 @@\n-a\n+b\n");
        let hunk = &diff.hunks[0];
        assert_eq!((hunk.old_lines, hunk.new_lines), (1, 1));
        assert_eq!(hunk.section, "");
        assert_eq!(hunk.header(), "@@ -1,1 +1,1 @@");
    }

    #[test]
    fn parses_several_hunks() {
        let diff = parse("@@ -1,2 +1,2 @@\n a\n-b\n+c\n@@ -50,1 +50,2 @@ fn later\n x\n+y\n");
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!(diff.hunks[1].new_start, 50);
        assert_eq!(diff.hunks[1].lines[1].new_number, Some(51));
    }

    #[test]
    fn marks_a_missing_trailing_newline_on_the_previous_line() {
        let diff = parse("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");
        let lines = &diff.hunks[0].lines;
        assert!(lines[0].no_newline_at_eof);
        assert!(!lines[1].no_newline_at_eof);
    }

    #[test]
    fn a_blank_context_line_with_its_space_stripped_is_kept() {
        let diff = parse("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n");
        assert_eq!(diff.hunks[0].lines[1].text, "");
        assert_eq!(diff.hunks[0].lines[1].kind, DiffLineKind::Context);
        assert_eq!(diff.hunks[0].lines[2].old_number, Some(3));
    }

    #[test]
    fn content_that_looks_like_a_header_is_not_mistaken_for_one() {
        // An added line whose text starts with "@@ " is "+@@ ...", so it stays an added line.
        let diff = parse("@@ -1 +1,2 @@\n a\n+@@ -9 +9 @@ not a header\n");
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].lines[1].text, "@@ -9 +9 @@ not a header");
    }

    #[test]
    fn detects_binary_files() {
        let diff =
            parse_diff(b"diff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n")
                .unwrap();
        assert!(diff.binary);
        assert!(diff.hunks.is_empty());
    }

    fn hunk_of(body: &str) -> DiffHunk {
        parse(body).hunks.remove(0)
    }

    #[test]
    fn patch_for_renders_a_hunk_git_apply_can_read() {
        let hunk = hunk_of("@@ -1,3 +1,3 @@ fn f\n a\n-b\n+B\n c\n");
        assert_eq!(
            patch_for("src/f.rs", &[&hunk]),
            "diff --git a/src/f.rs b/src/f.rs\n--- a/src/f.rs\n+++ b/src/f.rs\n\
             @@ -1,3 +1,3 @@ fn f\n a\n-b\n+B\n c\n"
        );
    }

    #[test]
    fn patch_for_keeps_the_missing_newline_marker_after_its_line() {
        let hunk = hunk_of("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");
        assert!(patch_for("f", &[&hunk]).ends_with("-a\n\\ No newline at end of file\n+b\n"));
    }

    #[test]
    fn patch_for_puts_several_hunks_in_order() {
        let both = parse("@@ -1 +1 @@\n-a\n+b\n@@ -9 +9 @@\n-c\n+d\n");
        let hunks: Vec<_> = both.hunks.iter().collect();
        let patch = patch_for("f", &hunks);
        assert!(patch.find("@@ -1 ").unwrap_or(0) < patch.find("@@ -9").unwrap());
    }

    #[test]
    fn patch_paths_with_spaces_get_a_tab_and_odd_characters_are_quoted() {
        let hunk = hunk_of("@@ -1 +1 @@\n-a\n+b\n");
        assert!(patch_for("my notes.txt", &[&hunk]).contains("--- a/my notes.txt\t\n"));
        assert!(patch_for("q\"uote.txt", &[&hunk]).contains("--- \"a/q\\\"uote.txt\"\n"));
        assert!(patch_for("new\nline", &[&hunk]).contains("\"a/new\\nline\""));
        // Non-ASCII is fine unquoted.
        assert!(patch_for("é.txt", &[&hunk]).contains("--- a/é.txt\n"));
    }

    #[test]
    fn empty_output_is_an_empty_diff() {
        let diff = parse_diff(b"").unwrap();
        assert!(diff.hunks.is_empty());
        assert!(!diff.binary);
    }

    #[test]
    fn malformed_headers_are_errors_not_panics() {
        assert!(parse_diff(b"@@ nonsense @@\n").is_err());
        assert!(parse_diff(b"@@ -x +1 @@\n").is_err());
        assert!(parse_diff(b"@@@ -1,2 -1,2 +1,3 @@@\n").is_err());
    }
}
