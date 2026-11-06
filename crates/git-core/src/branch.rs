use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchKind {
    Local,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// The short name: `feat/x` for a local branch, `origin/feat/x` for a remote one.
    pub name: String,
    pub kind: BranchKind,
    pub is_current: bool,
    /// When the branch's tip was committed, as Unix seconds.
    pub last_commit: Option<i64>,
    pub upstream: Option<String>,
}

/// Branches ready to show: local ones, then remote ones that have no local counterpart,
/// each newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchList {
    pub local: Vec<Branch>,
    pub remote_only: Vec<Branch>,
}

/// Arguments for `git for-each-ref`, newest first.
pub(crate) const LIST_ARGS: &[&str] = &[
    "for-each-ref",
    "--sort=-committerdate",
    "--format=%(refname)%00%(refname:short)%00%(HEAD)%00%(committerdate:unix)%00%(upstream:short)",
    "refs/heads",
    "refs/remotes",
];

/// Parses the output of `git for-each-ref` with the format in [`LIST_ARGS`].
pub(crate) fn parse_branches(output: &[u8]) -> Result<BranchList, Error> {
    let text = String::from_utf8_lossy(output);
    let mut local = Vec::new();
    let mut remote = Vec::new();

    for line in text.lines().filter(|line| !line.is_empty()) {
        let fields: Vec<&str> = line.split('\0').collect();
        let [refname, short, head, date, upstream] = fields[..] else {
            return Err(Error::Parse(format!("bad branch line {line:?}")));
        };
        // `origin/HEAD` is a pointer to another branch, not a branch of its own.
        if refname.ends_with("/HEAD") {
            continue;
        }
        let kind = if refname.starts_with("refs/heads/") {
            BranchKind::Local
        } else {
            BranchKind::Remote
        };
        let branch = Branch {
            name: short.to_owned(),
            kind,
            is_current: head == "*",
            last_commit: date.parse().ok(),
            upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
        };
        match kind {
            BranchKind::Local => local.push(branch),
            BranchKind::Remote => remote.push(branch),
        }
    }

    let remote_only = remote
        .into_iter()
        .filter(|branch| {
            let local_name = branch
                .name
                .split_once('/')
                .map_or(branch.name.as_str(), |(_, rest)| rest);
            !local.iter().any(|local| local.name == local_name)
        })
        .collect();
    Ok(BranchList { local, remote_only })
}

/// A name git would accept for a new branch, or an explanation-free `false`.
/// The real check is `git check-ref-format`; this only rejects what must never reach
/// the command line, such as an option.
pub(crate) fn is_safe_branch_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && !name.contains('\0') && !name.contains('\n')
}

/// "Just now", "12 min ago", "Yesterday", "3 weeks ago": how long before `now` something
/// happened, for lists of branches and commits.
pub fn relative_time(then: i64, now: i64) -> String {
    let seconds = (now - then).max(0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match seconds {
        0..=44 => "Just now".to_owned(),
        // "min" is an abbreviation, so it takes no plural: "12 min ago".
        45..=3_599 => format!(
            "{} min ago",
            (seconds as f64 / 60.0).round().max(1.0) as i64
        ),
        3_600..=86_399 => plural(seconds / 3_600, "hour"),
        86_400..=172_799 => "Yesterday".to_owned(),
        172_800..=1_209_599 => plural(seconds / 86_400, "day"),
        1_209_600..=5_183_999 => plural(seconds / 604_800, "week"),
        5_184_000..=31_535_999 => plural(seconds / 2_592_000, "month"),
        _ => plural(seconds / 31_536_000, "year"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(refname: &str, short: &str, head: &str, date: &str, upstream: &str) -> String {
        format!("{refname}\0{short}\0{head}\0{date}\0{upstream}\n")
    }

    #[test]
    fn splits_local_and_remote_and_marks_the_current_branch() {
        let output = [
            line("refs/heads/feat/x", "feat/x", "*", "300", "origin/feat/x"),
            line("refs/heads/main", "main", " ", "200", ""),
            line("refs/remotes/origin/other", "origin/other", " ", "100", ""),
        ]
        .concat();

        let list = parse_branches(output.as_bytes()).unwrap();
        assert_eq!(list.local.len(), 2);
        assert!(list.local[0].is_current);
        assert_eq!(list.local[0].upstream.as_deref(), Some("origin/feat/x"));
        assert_eq!(list.local[1].upstream, None);
        assert_eq!(list.remote_only.len(), 1);
        assert_eq!(list.remote_only[0].name, "origin/other");
        assert_eq!(list.remote_only[0].kind, BranchKind::Remote);
    }

    #[test]
    fn a_remote_branch_with_a_local_twin_is_not_listed_twice() {
        let output = [
            line("refs/heads/main", "main", " ", "200", "origin/main"),
            line("refs/remotes/origin/main", "origin/main", " ", "200", ""),
            line(
                "refs/remotes/origin/feat/deep/name",
                "origin/feat/deep/name",
                " ",
                "100",
                "",
            ),
        ]
        .concat();

        let list = parse_branches(output.as_bytes()).unwrap();
        let names: Vec<_> = list.remote_only.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["origin/feat/deep/name"]);
    }

    #[test]
    fn the_remote_head_pointer_is_not_a_branch() {
        let output = line("refs/remotes/origin/HEAD", "origin", " ", "100", "");
        let list = parse_branches(output.as_bytes()).unwrap();
        assert!(list.local.is_empty() && list.remote_only.is_empty());
    }

    #[test]
    fn a_missing_date_is_tolerated() {
        let output = line("refs/heads/odd", "odd", " ", "", "");
        let list = parse_branches(output.as_bytes()).unwrap();
        assert_eq!(list.local[0].last_commit, None);
    }

    #[test]
    fn a_malformed_line_is_an_error_not_a_panic() {
        assert!(parse_branches(b"refs/heads/x\0x\n").is_err());
    }

    #[test]
    fn names_that_could_be_options_are_unsafe() {
        assert!(is_safe_branch_name("feat/x"));
        assert!(!is_safe_branch_name("--force"));
        assert!(!is_safe_branch_name("-b"));
        assert!(!is_safe_branch_name(""));
        assert!(!is_safe_branch_name("a\nb"));
    }

    #[test]
    fn relative_times_read_naturally() {
        let now = 1_000_000_000;
        let ago = |seconds| relative_time(now - seconds, now);
        assert_eq!(ago(5), "Just now");
        assert_eq!(ago(60), "1 min ago");
        assert_eq!(ago(12 * 60), "12 min ago");
        assert_eq!(ago(3_600), "1 hour ago");
        assert_eq!(ago(5 * 3_600), "5 hours ago");
        assert_eq!(ago(30 * 3_600), "Yesterday");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(ago(15 * 86_400), "2 weeks ago");
        assert_eq!(ago(90 * 86_400), "3 months ago");
        assert_eq!(ago(800 * 86_400), "2 years ago");
    }

    #[test]
    fn a_time_in_the_future_is_just_now() {
        assert_eq!(relative_time(2_000, 1_000), "Just now");
    }
}
