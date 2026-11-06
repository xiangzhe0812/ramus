use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Conflicted,
    Untracked,
}

/// What HEAD points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// On a branch. The branch may be unborn (no commits yet); see `BranchStatus::oid`.
    Branch(String),
    /// Detached at a commit; holds the abbreviated hash.
    Detached(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchStatus {
    pub head: Head,
    /// Full hash of the current commit, `None` on an unborn branch.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// The previous path, for renames and copies.
    pub original_path: Option<String>,
    /// Change recorded in the index (staged), if any.
    pub index: Option<ChangeKind>,
    /// Change in the working tree not yet staged, if any.
    pub worktree: Option<ChangeKind>,
}

impl FileChange {
    /// The single kind to show for this file: a conflict wins, then the staged
    /// change, then the working-tree change.
    pub fn kind(&self) -> ChangeKind {
        if self.index == Some(ChangeKind::Conflicted)
            || self.worktree == Some(ChangeKind::Conflicted)
        {
            return ChangeKind::Conflicted;
        }
        self.index.or(self.worktree).unwrap_or(ChangeKind::Modified)
    }

    /// Whether the file has a change staged for the next commit.
    pub fn is_staged(&self) -> bool {
        self.index.is_some()
    }

    /// Whether everything changed in this file is staged (no unstaged edits left).
    pub fn is_fully_staged(&self) -> bool {
        self.index.is_some() && self.worktree.is_none()
    }

    /// Whether the file is a plain modified file on both sides, the only case where
    /// staging or unstaging a single hunk is well defined. Added, deleted, renamed,
    /// untracked and conflicted files are staged as a whole.
    fn is_plain_modification(&self) -> bool {
        use ChangeKind::*;
        self.original_path.is_none()
            && !matches!(
                self.index,
                Some(Deleted | Renamed | Copied | TypeChanged | Conflicted)
            )
            && !matches!(
                self.worktree,
                Some(Deleted | Renamed | Copied | TypeChanged | Conflicted | Untracked)
            )
    }

    /// Whether a hunk of the unstaged changes can be staged on its own.
    pub fn can_stage_hunks(&self) -> bool {
        self.is_plain_modification() && self.worktree == Some(ChangeKind::Modified)
    }

    /// Whether a hunk of the staged changes can be unstaged on its own. A file added
    /// in the index is excluded: reversing part of "this file is new" is not meaningful.
    pub fn can_unstage_hunks(&self) -> bool {
        self.is_plain_modification() && self.index == Some(ChangeKind::Modified)
    }

    /// The paths git needs to act on this file as one change: for a rename that is
    /// both the new and the old path.
    pub fn pathspecs(&self) -> Vec<&str> {
        let mut paths = vec![self.path.as_str()];
        paths.extend(self.original_path.as_deref());
        paths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: BranchStatus,
    /// Changed files sorted by path.
    pub files: Vec<FileChange>,
}

fn change_kind(code: char) -> Result<Option<ChangeKind>, Error> {
    Ok(match code {
        '.' => None,
        'M' => Some(ChangeKind::Modified),
        'A' => Some(ChangeKind::Added),
        'D' => Some(ChangeKind::Deleted),
        'R' => Some(ChangeKind::Renamed),
        'C' => Some(ChangeKind::Copied),
        'T' => Some(ChangeKind::TypeChanged),
        other => return Err(Error::Parse(format!("unknown status code {other:?}"))),
    })
}

fn xy(field: &str) -> Result<(Option<ChangeKind>, Option<ChangeKind>), Error> {
    let mut chars = field.chars();
    match (chars.next(), chars.next()) {
        (Some(x), Some(y)) => Ok((change_kind(x)?, change_kind(y)?)),
        _ => Err(Error::Parse(format!("bad XY field {field:?}"))),
    }
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub(crate) fn parse_status(output: &[u8]) -> Result<RepoStatus, Error> {
    let text = String::from_utf8_lossy(output);
    let mut records = text.split('\0').filter(|record| !record.is_empty());

    let mut head = None;
    let mut oid = None;
    let mut upstream = None;
    let (mut ahead, mut behind) = (0, 0);
    let mut files = Vec::new();

    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => oid = (value != "(initial)").then(|| value.to_owned()),
                "branch.head" => head = Some(value.to_owned()),
                "branch.upstream" => upstream = Some(value.to_owned()),
                "branch.ab" => {
                    let parse = |part: Option<&str>, sign: char| {
                        part.and_then(|part| part.strip_prefix(sign))
                            .and_then(|count| count.parse::<u32>().ok())
                    };
                    let mut parts = value.split(' ');
                    ahead = parse(parts.next(), '+')
                        .ok_or_else(|| Error::Parse(format!("bad branch.ab {value:?}")))?;
                    behind = parse(parts.next(), '-')
                        .ok_or_else(|| Error::Parse(format!("bad branch.ab {value:?}")))?;
                }
                _ => {}
            }
            continue;
        }

        let kind = record.chars().next();
        match kind {
            // Ordinary change: 1 XY sub mH mI mW hH hI path
            Some('1') => {
                let fields: Vec<&str> = record.splitn(9, ' ').collect();
                let [_, status, .., path] = fields[..] else {
                    return Err(Error::Parse(format!("bad ordinary entry {record:?}")));
                };
                let (index, worktree) = xy(status)?;
                files.push(FileChange {
                    path: path.to_owned(),
                    original_path: None,
                    index,
                    worktree,
                });
            }
            // Rename or copy: 2 XY sub mH mI mW hH hI Xscore path, then the original path
            // as its own NUL-separated record.
            Some('2') => {
                let fields: Vec<&str> = record.splitn(10, ' ').collect();
                let [_, status, .., path] = fields[..] else {
                    return Err(Error::Parse(format!("bad rename entry {record:?}")));
                };
                let original = records
                    .next()
                    .ok_or_else(|| Error::Parse("rename entry without original path".into()))?;
                let (index, worktree) = xy(status)?;
                files.push(FileChange {
                    path: path.to_owned(),
                    original_path: Some(original.to_owned()),
                    index,
                    worktree,
                });
            }
            // Unmerged: u XY sub m1 m2 m3 mW h1 h2 h3 path
            Some('u') => {
                let path = record
                    .splitn(11, ' ')
                    .nth(10)
                    .ok_or_else(|| Error::Parse(format!("bad unmerged entry {record:?}")))?;
                files.push(FileChange {
                    path: path.to_owned(),
                    original_path: None,
                    index: Some(ChangeKind::Conflicted),
                    worktree: Some(ChangeKind::Conflicted),
                });
            }
            Some('?') => {
                let path = record
                    .strip_prefix("? ")
                    .ok_or_else(|| Error::Parse(format!("bad untracked entry {record:?}")))?;
                files.push(FileChange {
                    path: path.to_owned(),
                    original_path: None,
                    index: None,
                    worktree: Some(ChangeKind::Untracked),
                });
            }
            // Ignored files are not requested, so skip them if they appear.
            Some('!') => {}
            _ => return Err(Error::Parse(format!("unknown record {record:?}"))),
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let head = match head.as_deref() {
        Some("(detached)") => {
            let oid = oid
                .as_deref()
                .ok_or_else(|| Error::Parse("detached HEAD without a commit".into()))?;
            Head::Detached(oid.chars().take(7).collect())
        }
        Some(name) => Head::Branch(name.to_owned()),
        None => return Err(Error::Parse("missing branch.head".into())),
    };

    Ok(RepoStatus {
        branch: BranchStatus {
            head,
            oid,
            upstream,
            ahead,
            behind,
        },
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(records: &[&str]) -> Result<RepoStatus, Error> {
        parse_status(format!("{}\0", records.join("\0")).as_bytes())
    }

    const OID: &str = "a41f9c2e0b7d4f1e8c3a5b6d7e8f9012a3b4c5d6";

    #[test]
    fn parses_branch_upstream_and_ahead_behind() {
        let status = parse(&[
            &format!("# branch.oid {OID}"),
            "# branch.head feat/diff-view",
            "# branch.upstream origin/feat/diff-view",
            "# branch.ab +2 -1",
        ])
        .unwrap();

        assert_eq!(status.branch.head, Head::Branch("feat/diff-view".into()));
        assert_eq!(status.branch.oid.as_deref(), Some(OID));
        assert_eq!(
            status.branch.upstream.as_deref(),
            Some("origin/feat/diff-view")
        );
        assert_eq!((status.branch.ahead, status.branch.behind), (2, 1));
        assert!(status.files.is_empty());
    }

    #[test]
    fn unborn_branch_has_no_oid() {
        let status = parse(&["# branch.oid (initial)", "# branch.head main"]).unwrap();
        assert_eq!(status.branch.head, Head::Branch("main".into()));
        assert_eq!(status.branch.oid, None);
        assert_eq!(status.branch.upstream, None);
    }

    #[test]
    fn detached_head_is_abbreviated() {
        let status = parse(&[&format!("# branch.oid {OID}"), "# branch.head (detached)"]).unwrap();
        assert_eq!(status.branch.head, Head::Detached("a41f9c2".into()));
    }

    #[test]
    fn parses_ordinary_entries_by_staged_state() {
        let h = "1111111111111111111111111111111111111111";
        let status = parse(&[
            "# branch.oid (initial)",
            "# branch.head main",
            &format!("1 M. N... 100644 100644 100644 {h} {h} src/staged.rs"),
            &format!("1 .M N... 100644 100644 100644 {h} {h} src/unstaged.rs"),
            &format!("1 AM N... 000000 100644 100644 {h} {h} src/both.rs"),
            &format!("1 .D N... 100644 100644 000000 {h} {h} src/gone.rs"),
        ])
        .unwrap();

        let by_path = |path: &str| status.files.iter().find(|f| f.path == path).unwrap();
        assert_eq!(by_path("src/staged.rs").index, Some(ChangeKind::Modified));
        assert_eq!(by_path("src/staged.rs").worktree, None);
        assert!(by_path("src/staged.rs").is_staged());
        assert!(!by_path("src/unstaged.rs").is_staged());
        assert_eq!(by_path("src/both.rs").kind(), ChangeKind::Added);
        assert_eq!(by_path("src/gone.rs").kind(), ChangeKind::Deleted);
    }

    #[test]
    fn paths_with_spaces_are_kept_whole() {
        let h = "1111111111111111111111111111111111111111";
        let status = parse(&[
            "# branch.oid (initial)",
            "# branch.head main",
            &format!("1 .M N... 100644 100644 100644 {h} {h} my notes/read me.md"),
            "? new file.txt",
        ])
        .unwrap();
        assert_eq!(status.files[0].path, "my notes/read me.md");
        assert_eq!(status.files[1].path, "new file.txt");
    }

    #[test]
    fn rename_consumes_the_original_path_record() {
        let h = "1111111111111111111111111111111111111111";
        let status = parse(&[
            "# branch.oid (initial)",
            "# branch.head main",
            &format!("2 R. N... 100644 100644 100644 {h} {h} R100 src/new_name.rs"),
            "src/old_name.rs",
            "? after.txt",
        ])
        .unwrap();

        assert_eq!(status.files.len(), 2);
        let renamed = &status.files[1];
        assert_eq!(renamed.path, "src/new_name.rs");
        assert_eq!(renamed.original_path.as_deref(), Some("src/old_name.rs"));
        assert_eq!(renamed.kind(), ChangeKind::Renamed);
        assert_eq!(status.files[0].path, "after.txt");
    }

    #[test]
    fn unmerged_entry_is_a_conflict() {
        let h = "1111111111111111111111111111111111111111";
        let status = parse(&[
            "# branch.oid (initial)",
            "# branch.head main",
            &format!("u UU N... 100644 100644 100644 100644 {h} {h} {h} conflicted.rs"),
        ])
        .unwrap();
        assert_eq!(status.files[0].kind(), ChangeKind::Conflicted);
    }

    fn change(
        index: Option<ChangeKind>,
        worktree: Option<ChangeKind>,
        original: Option<&str>,
    ) -> FileChange {
        FileChange {
            path: "f".into(),
            original_path: original.map(str::to_owned),
            index,
            worktree,
        }
    }

    #[test]
    fn only_plain_modifications_support_hunk_staging() {
        use ChangeKind::*;
        let m = Some(Modified);
        assert!(change(None, m, None).can_stage_hunks());
        assert!(change(m, m, None).can_stage_hunks());
        assert!(change(Some(Added), m, None).can_stage_hunks());
        assert!(!change(None, Some(Untracked), None).can_stage_hunks());
        assert!(!change(None, Some(Deleted), None).can_stage_hunks());
        assert!(!change(Some(Renamed), m, Some("old")).can_stage_hunks());
        assert!(!change(Some(Conflicted), Some(Conflicted), None).can_stage_hunks());

        assert!(change(m, None, None).can_unstage_hunks());
        assert!(change(m, m, None).can_unstage_hunks());
        assert!(!change(Some(Added), None, None).can_unstage_hunks());
        assert!(!change(Some(Deleted), None, None).can_unstage_hunks());
        assert!(!change(Some(Renamed), None, Some("old")).can_unstage_hunks());
    }

    #[test]
    fn files_are_sorted_by_path() {
        let h = "1111111111111111111111111111111111111111";
        let status = parse(&[
            "# branch.oid (initial)",
            "# branch.head main",
            &format!("1 .M N... 100644 100644 100644 {h} {h} z.rs"),
            "? a.txt",
        ])
        .unwrap();
        let paths: Vec<_> = status.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "z.rs"]);
    }

    #[test]
    fn malformed_output_is_an_error_not_a_panic() {
        assert!(parse(&["# branch.oid (initial)", "# branch.head main", "1 garbage"]).is_err());
        assert!(parse(&["# branch.oid (initial)"]).is_err());
        assert!(parse(&["# branch.oid (initial)", "# branch.head main", "Z what"]).is_err());
    }
}
