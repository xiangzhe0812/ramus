//! What the command palette offers and how a typed query narrows it. No UI in here, so
//! the rules are testable on their own.

use git_core::Branch;

use crate::repo_state::SyncKind;

/// Something the palette can do. Most map onto an action the keyboard already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenRepository,
    CloneRepository,
    SwitchRepository,
    ShowAccounts,
    SwitchBranch,
    SwitchWorktree,
    ShowChanges,
    ShowHistory,
    ShowStashes,
    ShowWorktrees,
    StashChanges,
    Sync(SyncKind),
    StageAll,
    UnstageAll,
    DiscardSelected,
}

/// What running an entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Command(Command),
    SwitchTo(Branch),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    /// The keyboard shortcut for this, if it has one.
    pub shortcut: Option<&'static str>,
    pub target: Target,
}

impl Entry {
    fn command(label: impl Into<String>, shortcut: Option<&'static str>, command: Command) -> Self {
        Self {
            label: label.into(),
            shortcut,
            target: Target::Command(command),
        }
    }

    pub fn is_branch(&self) -> bool {
        matches!(self.target, Target::SwitchTo(_))
    }
}

/// What is true right now, which decides which commands make sense.
pub struct Situation<'a> {
    pub has_repo: bool,
    /// Repositories are remembered, so there is something to switch to.
    pub has_recent: bool,
    /// A git operation is already running, so another can't start.
    pub busy: bool,
    /// The sync the title-bar button would run, and its label ("Push origin").
    pub primary_sync: Option<(SyncKind, String)>,
    pub local: &'a [Branch],
    pub remote_only: &'a [Branch],
}

/// Most branches shown at once, so a short query in a big repo stays scannable.
const MAX_BRANCHES: usize = 6;

/// Every word of `query` appears in `text`, in any order, ignoring case.
fn matches(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

/// Entries whose label starts with the query come before ones that merely contain it.
fn starts_with_query(label: &str, query: &str) -> bool {
    label
        .to_lowercase()
        .starts_with(&query.trim().to_lowercase())
}

fn all_commands(situation: &Situation) -> Vec<Entry> {
    let mut entries = vec![Entry::command(
        "Open repository…",
        Some("⌘O"),
        Command::OpenRepository,
    )];
    entries.push(Entry::command(
        "Clone repository…",
        Some("⇧⌘O"),
        Command::CloneRepository,
    ));
    if situation.has_recent {
        entries.push(Entry::command(
            "Switch repository…",
            Some("⌘T"),
            Command::SwitchRepository,
        ));
    }
    entries.push(Entry::command("Accounts…", None, Command::ShowAccounts));
    if !situation.has_repo {
        return entries;
    }
    entries.push(Entry::command(
        "Switch branch…",
        Some("⌘B"),
        Command::SwitchBranch,
    ));
    entries.push(Entry::command(
        "Switch worktree…",
        Some("⇧⌘B"),
        Command::SwitchWorktree,
    ));
    entries.push(Entry::command(
        "Show changes",
        Some("⌘1"),
        Command::ShowChanges,
    ));
    entries.push(Entry::command(
        "Show history",
        Some("⌘2"),
        Command::ShowHistory,
    ));
    entries.push(Entry::command(
        "Show stashes",
        Some("⌘3"),
        Command::ShowStashes,
    ));
    entries.push(Entry::command(
        "Show worktrees",
        Some("⌘4"),
        Command::ShowWorktrees,
    ));
    entries.push(Entry::command("Stash changes", None, Command::StashChanges));
    if !situation.busy {
        let primary = situation.primary_sync.as_ref();
        if let Some((kind, label)) = primary {
            entries.push(Entry::command(label.clone(), None, Command::Sync(*kind)));
        }
        if primary.is_none_or(|(kind, _)| *kind != SyncKind::Fetch) {
            entries.push(Entry::command(
                "Fetch origin",
                None,
                Command::Sync(SyncKind::Fetch),
            ));
        }
    }
    entries.push(Entry::command("Stage all changes", None, Command::StageAll));
    entries.push(Entry::command(
        "Unstage all changes",
        None,
        Command::UnstageAll,
    ));
    entries.push(Entry::command(
        "Discard selected changes…",
        Some("⌘⌫"),
        Command::DiscardSelected,
    ));
    entries
}

/// The entries to list for `query`: matching commands, then (only once something is
/// typed) matching branches the user could switch to.
pub fn entries(situation: &Situation, query: &str) -> Vec<Entry> {
    let query = query.trim();
    let mut commands: Vec<_> = all_commands(situation)
        .into_iter()
        .filter(|entry| matches(&entry.label, query))
        .collect();
    // A stable sort keeps the natural order within each group.
    commands.sort_by_key(|entry| !starts_with_query(&entry.label, query));

    if query.is_empty() || !situation.has_repo {
        return commands;
    }
    let branches = situation
        .local
        .iter()
        .chain(situation.remote_only)
        .filter(|branch| !branch.is_current && matches(&branch.name, query))
        .take(MAX_BRANCHES)
        .map(|branch| Entry {
            label: branch.name.clone(),
            shortcut: None,
            target: Target::SwitchTo(branch.clone()),
        });
    commands.extend(branches);
    commands
}

/// The position of each entry among the list's rows, counting the section titles
/// ("COMMANDS" before the first command, "BRANCHES" before the first branch) as rows too.
/// Scrolling the highlighted entry into view needs this, because the titles are rows.
pub fn row_positions(entries: &[Entry]) -> Vec<usize> {
    let mut positions = Vec::with_capacity(entries.len());
    let mut rows = 0;
    let mut in_branches = false;
    for (ix, entry) in entries.iter().enumerate() {
        if ix == 0 && !entry.is_branch() {
            rows += 1; // the COMMANDS title
        }
        if entry.is_branch() && !in_branches {
            in_branches = true;
            rows += 1; // the BRANCHES title
        }
        positions.push(rows);
        rows += 1;
    }
    positions
}

/// Where the selection goes after moving by `delta`, wrapping around the ends.
pub fn move_selection(selected: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as isize + delta).rem_euclid(len as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_core::BranchKind;

    fn branch(name: &str, is_current: bool, kind: BranchKind) -> Branch {
        Branch {
            name: name.into(),
            kind,
            is_current,
            last_commit: None,
            upstream: None,
        }
    }

    fn situation<'a>(local: &'a [Branch], remote_only: &'a [Branch]) -> Situation<'a> {
        Situation {
            has_repo: true,
            has_recent: false,
            busy: false,
            primary_sync: Some((SyncKind::Push, "Push origin".into())),
            local,
            remote_only,
        }
    }

    fn labels(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.label.as_str()).collect()
    }

    #[test]
    fn without_a_repository_only_repository_and_account_commands_are_offered() {
        let mut s = situation(&[], &[]);
        s.has_repo = false;
        assert_eq!(
            labels(&entries(&s, "")),
            ["Open repository…", "Clone repository…", "Accounts…"]
        );
    }

    #[test]
    fn an_empty_query_lists_commands_but_no_branches() {
        let local = [branch("main", true, BranchKind::Local)];
        let all = entries(&situation(&local, &[]), "");
        assert!(all.iter().any(|e| e.label == "Switch branch…"));
        assert!(all.iter().all(|e| !e.is_branch()));
    }

    #[test]
    fn switch_repository_is_offered_once_something_is_remembered() {
        let mut s = situation(&[], &[]);
        s.has_repo = false;
        assert!(!labels(&entries(&s, "")).contains(&"Switch repository…"));

        s.has_recent = true;
        let all = entries(&s, "");
        let entry = all
            .iter()
            .find(|entry| entry.label == "Switch repository…")
            .unwrap();
        assert_eq!(entry.shortcut, Some("⌘T"));
        assert_eq!(entry.target, Target::Command(Command::SwitchRepository));
    }

    #[test]
    fn switch_worktree_is_offered_with_a_repository() {
        let all = entries(&situation(&[], &[]), "");

        let branch = all
            .iter()
            .position(|entry| entry.label == "Switch branch…")
            .unwrap();
        let worktree = &all[branch + 1];
        assert_eq!(worktree.label, "Switch worktree…");
        assert_eq!(worktree.shortcut, Some("⇧⌘B"));
        assert_eq!(worktree.target, Target::Command(Command::SwitchWorktree));
    }

    #[test]
    fn a_query_matches_every_word_in_any_order_ignoring_case() {
        let s = situation(&[], &[]);
        assert_eq!(
            labels(&entries(&s, "CHANGES unstage")),
            ["Unstage all changes"]
        );
        assert!(entries(&s, "zzz").is_empty());
    }

    #[test]
    fn commands_starting_with_the_query_come_first() {
        let s = situation(&[], &[]);
        // Many labels contain "st" (Show stashes, Unstage all changes…); only some start
        // with it, and those must all come before the ones that merely contain it.
        let found = entries(&s, "st");
        let starts = |label: &str| label.to_lowercase().starts_with("st");
        let leading = found.iter().take_while(|e| starts(&e.label)).count();
        assert!(leading >= 2, "{:?}", labels(&found));
        assert!(found[leading..].iter().all(|e| !starts(&e.label)));
        assert!(labels(&found[..leading]).contains(&"Stage all changes"));
    }

    #[test]
    fn typing_offers_other_branches_but_never_the_current_one() {
        let local = [
            branch("main", true, BranchKind::Local),
            branch("feat/stash", false, BranchKind::Local),
        ];
        let remote = [branch("origin/feat/stage", false, BranchKind::Remote)];
        let found = entries(&situation(&local, &remote), "feat");
        let branches: Vec<_> = found.iter().filter(|e| e.is_branch()).collect();
        assert_eq!(
            labels(&branches.iter().map(|e| (*e).clone()).collect::<Vec<_>>()),
            ["feat/stash", "origin/feat/stage"]
        );
        assert!(
            entries(&situation(&local, &remote), "main")
                .iter()
                .all(|e| !e.is_branch())
        );
    }

    #[test]
    fn only_a_handful_of_branches_are_listed() {
        let local: Vec<_> = (0..20)
            .map(|n| branch(&format!("topic-{n}"), false, BranchKind::Local))
            .collect();
        let found = entries(&situation(&local, &[]), "topic");
        assert_eq!(found.len(), MAX_BRANCHES);
    }

    #[test]
    fn sync_is_offered_once_and_not_while_busy() {
        let s = situation(&[], &[]);
        let found = entries(&s, "origin");
        assert_eq!(labels(&found), ["Push origin", "Fetch origin"]);

        let mut fetching = situation(&[], &[]);
        fetching.primary_sync = Some((SyncKind::Fetch, "Fetch origin".into()));
        assert_eq!(labels(&entries(&fetching, "origin")), ["Fetch origin"]);

        let mut busy = situation(&[], &[]);
        busy.busy = true;
        assert!(entries(&busy, "origin").is_empty());
    }

    #[test]
    fn titles_count_as_rows_when_finding_where_an_entry_sits() {
        let command = |label: &str| Entry::command(label, None, Command::StageAll);
        let branch = |name: &str| Entry {
            label: name.into(),
            shortcut: None,
            target: Target::SwitchTo(branch(name, false, BranchKind::Local)),
        };
        // COMMANDS, a, b, BRANCHES, x, y
        let entries = [command("a"), command("b"), branch("x"), branch("y")];
        assert_eq!(row_positions(&entries), [1, 2, 4, 5]);
        // Only branches: just the BRANCHES title in front.
        assert_eq!(row_positions(&[branch("x"), branch("y")]), [1, 2]);
        assert!(row_positions(&[]).is_empty());
    }

    #[test]
    fn selection_wraps_around_both_ends() {
        assert_eq!(move_selection(0, -1, 3), 2);
        assert_eq!(move_selection(2, 1, 3), 0);
        assert_eq!(move_selection(1, 1, 3), 2);
        assert_eq!(move_selection(0, 1, 0), 0);
    }
}
