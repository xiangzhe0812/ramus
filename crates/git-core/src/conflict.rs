//! Merge conflicts: noticing that an operation stopped for them, reading the conflicted
//! files, resolving them, and finishing or abandoning the operation.

use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
};

use crate::{
    Error, Repo,
    discard::checked_path,
    runner::{run_git, run_git_with_env},
};

/// Which operation stopped for conflicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Merge,
    CherryPick,
    Revert,
}

impl OperationKind {
    /// The git subcommand: `merge`, `cherry-pick` or `revert`.
    fn command(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

/// An operation that has started and is waiting to be finished or abandoned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InProgress {
    pub kind: OperationKind,
    /// For a merge, the branch being merged in, when git's message names one.
    pub merging: Option<String>,
}

/// How a file is in conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed or added it: the file holds conflict markers.
    BothChanged,
    /// We changed it, they deleted it.
    DeletedByThem,
    /// They changed it, we deleted it.
    DeletedByUs,
    BothDeleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

/// What to keep from one conflicting region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Ours,
    Theirs,
    /// Ours, then theirs.
    Both,
}

/// One `<<<<<<<` … `>>>>>>>` block in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRegion {
    /// Line indexes (0-based) of the first and last marker line.
    pub first_line: usize,
    pub last_line: usize,
    pub ours_label: String,
    pub theirs_label: String,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

fn marker(line: &str, symbol: &str) -> bool {
    let line = line.trim_end_matches(['\n', '\r']);
    let seven = symbol.repeat(7);
    line == seven || line.starts_with(&format!("{seven} "))
}

fn marker_label(line: &str) -> String {
    line.trim_end_matches(['\n', '\r'])
        .get(8..)
        .unwrap_or_default()
        .to_owned()
}

/// The conflicting regions in `text`, in order. Handles the `diff3` style, whose
/// `|||||||` base section is skipped. A block that never closes is not a region.
pub fn conflict_regions(text: &str) -> Vec<ConflictRegion> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut regions = Vec::new();
    let mut ix = 0;
    while ix < lines.len() {
        if !marker(lines[ix], "<") {
            ix += 1;
            continue;
        }
        let first_line = ix;
        let (mut ours, mut theirs) = (Vec::new(), Vec::new());
        enum Part {
            Ours,
            Base,
            Theirs,
        }
        let mut part = Part::Ours;
        let mut closed = None;
        for (offset, line) in lines[ix + 1..].iter().enumerate() {
            let at = ix + 1 + offset;
            if marker(line, "<") {
                break; // A new block starts before this one closed; this one is broken.
            }
            match part {
                Part::Ours if marker(line, "|") => part = Part::Base,
                Part::Ours if marker(line, "=") => part = Part::Theirs,
                Part::Base if marker(line, "=") => part = Part::Theirs,
                Part::Theirs if marker(line, ">") => {
                    closed = Some(at);
                    break;
                }
                Part::Ours => ours.push((*line).to_owned()),
                Part::Theirs => theirs.push((*line).to_owned()),
                Part::Base => {}
            }
        }
        match closed {
            Some(last_line) => {
                regions.push(ConflictRegion {
                    first_line,
                    last_line,
                    ours_label: marker_label(lines[first_line]),
                    theirs_label: marker_label(lines[last_line]),
                    ours,
                    theirs,
                });
                ix = last_line + 1;
            }
            None => ix += 1,
        }
    }
    regions
}

/// `text` with region number `index` replaced by what `choice` keeps. Anything out of
/// range leaves the text unchanged.
pub fn resolve_region(text: &str, index: usize, choice: Choice) -> String {
    let regions = conflict_regions(text);
    let Some(region) = regions.get(index) else {
        return text.to_owned();
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::new();
    for line in &lines[..region.first_line] {
        out.push_str(line);
    }
    let keep: Vec<&String> = match choice {
        Choice::Ours => region.ours.iter().collect(),
        Choice::Theirs => region.theirs.iter().collect(),
        Choice::Both => region.ours.iter().chain(&region.theirs).collect(),
    };
    for line in keep {
        out.push_str(line);
    }
    // A kept line that was the last in its block may lack a newline only if the file did.
    for line in &lines[region.last_line + 1..] {
        out.push_str(line);
    }
    out
}

impl Repo {
    fn git_path_exists(&self, name: &str) -> Result<bool, Error> {
        let output = run_git(self.root(), &["rev-parse", "--git-path", name])?;
        let path = std::path::PathBuf::from(String::from_utf8_lossy(&output).trim());
        let path = if path.is_absolute() {
            path
        } else {
            self.root().join(path)
        };
        Ok(path.exists())
    }

    /// The operation waiting for its conflicts to be dealt with, if any.
    pub fn in_progress(&self) -> Result<Option<InProgress>, Error> {
        let kind = if self.git_path_exists("MERGE_HEAD")? {
            OperationKind::Merge
        } else if self.git_path_exists("CHERRY_PICK_HEAD")? {
            OperationKind::CherryPick
        } else if self.git_path_exists("REVERT_HEAD")? {
            OperationKind::Revert
        } else {
            return Ok(None);
        };
        let merging = if kind == OperationKind::Merge {
            let output = run_git(self.root(), &["rev-parse", "--git-path", "MERGE_MSG"])?;
            let path = String::from_utf8_lossy(&output).trim().to_owned();
            let path = self.root().join(path);
            fs::read_to_string(path)
                .ok()
                .and_then(|message| message.lines().next().map(str::to_owned))
                .and_then(|line| {
                    // "Merge branch 'feature'" / "Merge remote-tracking branch 'origin/main'"
                    let start = line.find('\'')? + 1;
                    let end = line[start..].find('\'')? + start;
                    Some(line[start..end].to_owned())
                })
        } else {
            None
        };
        Ok(Some(InProgress { kind, merging }))
    }

    /// The paths currently in conflict.
    pub fn conflicted_paths(&self) -> Result<Vec<String>, Error> {
        Ok(self
            .status()?
            .files
            .into_iter()
            .filter(|file| file.kind() == crate::ChangeKind::Conflicted)
            .map(|file| file.path)
            .collect())
    }

    /// The error to return when an operation stopped: if it left conflicts to resolve,
    /// `Conflicts` (and the operation stays in progress); otherwise the operation is
    /// cleaned up and `fallback` explains what went wrong.
    pub(crate) fn stopped_or(&self, kind: OperationKind, fallback: Error) -> Error {
        let in_progress = self.in_progress().ok().flatten();
        let conflicted = self.conflicted_paths().unwrap_or_default();
        if in_progress.as_ref().is_some_and(|p| p.kind == kind) && !conflicted.is_empty() {
            return Error::Conflicts(conflicted);
        }
        if in_progress.is_some() {
            let _ = run_git(self.root(), &[kind.command(), "--abort"]);
        }
        fallback
    }

    /// Which way `path` is in conflict.
    pub fn conflict_kind(&self, path: &str) -> Result<ConflictKind, Error> {
        let output = run_git(self.root(), &["ls-files", "-u", "-z", "--", path])?;
        let text = String::from_utf8_lossy(&output);
        // Each entry is "<mode> <oid> <stage>\t<path>"; stage 2 is ours, 3 is theirs.
        let has_stage = |stage: char| {
            text.split('\0')
                .filter(|entry| !entry.is_empty())
                .any(|entry| {
                    entry.split('\t').next().and_then(|m| m.split(' ').nth(2))
                        == Some(&stage.to_string())
                })
        };
        Ok(match (has_stage('2'), has_stage('3')) {
            (true, true) => ConflictKind::BothChanged,
            (true, false) => ConflictKind::DeletedByThem,
            (false, true) => ConflictKind::DeletedByUs,
            (false, false) => ConflictKind::BothDeleted,
        })
    }

    /// The conflicted file as it is on disk, markers and all. `None` when it isn't text
    /// (or isn't there).
    pub fn conflict_text(&self, path: &str) -> Result<Option<String>, Error> {
        let full = checked_path(self.root(), path, false)?;
        // `checked_path` vets the folders on the way; the file itself must not be a link
        // (or anything but a plain file) either, or this would read wherever it points.
        let mut file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&full)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // ELOOP: the last component is a symlink. Not text we will show.
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8(bytes).ok())
    }

    fn require_conflicted(&self, path: &str) -> Result<(), Error> {
        if self.conflicted_paths()?.iter().any(|p| p == path) {
            Ok(())
        } else {
            Err(Error::Git {
                command: "resolve".into(),
                stderr: format!("{path} is not in conflict"),
            })
        }
    }

    /// Saves `text` as the resolved file and stages it.
    pub fn resolve_with_text(&self, path: &str, text: &str) -> Result<(), Error> {
        self.require_conflicted(path)?;
        let full = checked_path(self.root(), path, false)?;
        // Replace a plain file only. A link in the last place would be written through,
        // so it is refused (O_NOFOLLOW also closes the gap between this check and the write).
        if !fs::symlink_metadata(&full)?.is_file() {
            return Err(Error::UnsafePath(path.to_owned()));
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&full)
            .map_err(|error| match error.raw_os_error() {
                Some(libc::ELOOP) => Error::UnsafePath(path.to_owned()),
                _ => error.into(),
            })?;
        file.write_all(text.as_bytes())?;
        drop(file);
        run_git(self.root(), &["add", "--", path]).map(|_| ())
    }

    /// Takes one side's version of the whole file, and stages it.
    pub fn resolve_with_side(&self, path: &str, side: Side) -> Result<(), Error> {
        self.require_conflicted(path)?;
        checked_path(self.root(), path, false)?;
        let flag = match side {
            Side::Ours => "--ours",
            Side::Theirs => "--theirs",
        };
        run_git(self.root(), &["checkout", flag, "--", path])?;
        run_git(self.root(), &["add", "--", path]).map(|_| ())
    }

    /// Keeps a file one side deleted, as it stands, and stages it.
    pub fn resolve_keep(&self, path: &str) -> Result<(), Error> {
        self.require_conflicted(path)?;
        checked_path(self.root(), path, false)?;
        run_git(self.root(), &["add", "--", path]).map(|_| ())
    }

    /// Deletes a file one side deleted, and stages that.
    pub fn resolve_delete(&self, path: &str) -> Result<(), Error> {
        self.require_conflicted(path)?;
        checked_path(self.root(), path, false)?;
        run_git(self.root(), &["rm", "-q", "--", path]).map(|_| ())
    }

    /// Gives up on the operation, putting everything back as it was before it started.
    pub fn abort_operation(&self) -> Result<(), Error> {
        let Some(in_progress) = self.in_progress()? else {
            return Ok(());
        };
        run_git(self.root(), &[in_progress.kind.command(), "--abort"]).map(|_| ())
    }

    /// Finishes the operation once every conflict is resolved, committing the result.
    pub fn continue_operation(&self) -> Result<(), Error> {
        let Some(in_progress) = self.in_progress()? else {
            return Ok(());
        };
        let left = self.conflicted_paths()?;
        if !left.is_empty() {
            return Err(Error::Conflicts(left));
        }
        // No editor can open (there is no terminal), so accept git's prepared message.
        let editor = [("GIT_EDITOR", "true".as_ref())];
        let args: &[&str] = match in_progress.kind {
            OperationKind::Merge => &["commit", "--no-edit"],
            OperationKind::CherryPick => &["cherry-pick", "--continue"],
            OperationKind::Revert => &["revert", "--continue"],
        };
        run_git_with_env(self.root(), args, &editor).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "top\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> feature\nbottom\n";

    #[test]
    fn finds_a_region_with_its_sides_and_labels() {
        let regions = conflict_regions(TWO);
        assert_eq!(regions.len(), 1);
        let r = &regions[0];
        assert_eq!((r.first_line, r.last_line), (1, 5));
        assert_eq!(r.ours, ["mine\n"]);
        assert_eq!(r.theirs, ["theirs\n"]);
        assert_eq!(
            (r.ours_label.as_str(), r.theirs_label.as_str()),
            ("HEAD", "feature")
        );
    }

    #[test]
    fn skips_the_base_section_of_diff3_style() {
        let text = "<<<<<<< HEAD\nmine\n||||||| base\nold\n=======\ntheirs\n>>>>>>> x\n";
        let r = &conflict_regions(text)[0];
        assert_eq!(r.ours, ["mine\n"]);
        assert_eq!(r.theirs, ["theirs\n"]);
    }

    #[test]
    fn finds_several_regions_in_order() {
        let text = format!("{TWO}mid\n{TWO}");
        let regions = conflict_regions(&text);
        assert_eq!(regions.len(), 2);
        assert!(regions[0].first_line < regions[1].first_line);
    }

    #[test]
    fn an_unclosed_block_is_not_a_region() {
        assert!(conflict_regions("<<<<<<< HEAD\nmine\n=======\ntheirs\n").is_empty());
        assert!(conflict_regions("no conflicts here\n").is_empty());
    }

    #[test]
    fn lookalike_lines_are_not_markers() {
        let text = "a ======= b\n====\n<<<<<<<x\n";
        assert!(conflict_regions(text).is_empty());
        // A rule drawn with equals signs inside a real block must not end it early.
        let text = "<<<<<<< HEAD\nmine\n=======\ntheirs\n=========\n>>>>>>> f\n";
        let r = &conflict_regions(text)[0];
        assert_eq!(r.theirs, ["theirs\n", "=========\n"]);
    }

    #[test]
    fn resolving_a_region_keeps_what_was_chosen_and_the_rest() {
        assert_eq!(resolve_region(TWO, 0, Choice::Ours), "top\nmine\nbottom\n");
        assert_eq!(
            resolve_region(TWO, 0, Choice::Theirs),
            "top\ntheirs\nbottom\n"
        );
        assert_eq!(
            resolve_region(TWO, 0, Choice::Both),
            "top\nmine\ntheirs\nbottom\n"
        );
    }

    #[test]
    fn resolving_one_of_two_regions_leaves_the_other() {
        let text = format!("{TWO}mid\n{TWO}");
        let after = resolve_region(&text, 1, Choice::Theirs);
        assert_eq!(conflict_regions(&after).len(), 1);
        assert!(after.ends_with("theirs\nbottom\n"));
        assert!(after.starts_with("top\n<<<<<<< HEAD"));
    }

    #[test]
    fn an_out_of_range_region_changes_nothing() {
        assert_eq!(resolve_region(TWO, 5, Choice::Ours), TWO);
        assert_eq!(resolve_region("plain\n", 0, Choice::Ours), "plain\n");
    }

    #[test]
    fn windows_line_endings_are_understood_and_kept() {
        let text = "<<<<<<< HEAD\r\nmine\r\n=======\r\ntheirs\r\n>>>>>>> f\r\nend\r\n";
        assert_eq!(conflict_regions(text).len(), 1);
        assert_eq!(resolve_region(text, 0, Choice::Ours), "mine\r\nend\r\n");
    }

    #[test]
    fn an_empty_side_is_fine() {
        let text = "<<<<<<< HEAD\n=======\ntheirs\n>>>>>>> f\n";
        assert_eq!(resolve_region(text, 0, Choice::Ours), "");
        assert_eq!(resolve_region(text, 0, Choice::Theirs), "theirs\n");
    }
}
