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
