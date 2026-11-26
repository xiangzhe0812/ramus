use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use git_core::{
    CommitFile, CommitInfo, DiffOptions, FileDiffs, Head, LabelKind, LogScope, RefLabel,
    relative_time,
};
use gpui::{
    AnyElement, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, PathBuilder, Pixels, Point, Render, Rgba, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, fill, point,
    prelude::FluentBuilder, px, size, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    diff_rows::HunkControls,
    diff_view::{DiffContent, DiffInteraction, diff_body, message},
    graph::{Graph, GraphRow},
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    ui::panel,
};

/// How many commits are read at a time.
const PAGE: usize = 300;

/// One lane of the commit graph, in pixels.
const LANE_WIDTH: f32 = 10.;
/// Lanes past this many are cut off, so a wide graph doesn't squeeze out the commit text.
const MAX_LANES: usize = 8;
const DOT_RADIUS: f32 = 3.5;
const LINE_WIDTH: f32 = 1.5;
/// Labels shown on one commit row; the rest are counted ("+2").
const MAX_LABELS: usize = 2;
/// The commit list's rows, in pixels.
const COMMIT_ROW_HEIGHT: f32 = 48.;

/// A row of the commit list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Commit(usize),
    LoadMore,
}

/// The History tab: the commits on this branch, and what the selected one changed.
pub struct HistoryView {
    repo_state: Entity<RepoState>,
    filter: Entity<InputState>,
    commits: Vec<CommitInfo>,
    unpushed: HashSet<String>,
    /// The current branch, or every branch.
    scope: LogScope,
    /// The graph's lanes after the commits read so far, so the next page carries on.
    graph: Graph,
    /// The graph row for each entry of `commits`.
    graph_rows: Vec<GraphRow>,
    /// Branch and tag names, by the commit they point at.
    labels: HashMap<String, Vec<RefLabel>>,
    /// The branch's upstream exists, by git-core's rule, so "not unpushed" means pushed.
    has_upstream: bool,
    has_more: bool,
    /// The repository, commit and commits-ahead count the list was read for; a change
    /// means read it again (a push changes only the count, and what is unpushed).
    loaded_for: Option<(std::path::PathBuf, Option<String>, u32)>,
    list_generation: u64,
    selected: Option<String>,
    files: Vec<CommitFile>,
    selected_file: Option<usize>,
    diff: Option<DiffContent>,
    hide_whitespace: bool,
    detail_generation: u64,
    diff_generation: u64,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// What the selected commit's header offers for taking it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UndoOffer {
    /// The newest commit, not pushed: it can simply be removed.
    Undo,
    /// The newest commit, already pushed: only a revert undoes it safely.
    PushedHint,
    Nothing,
}

/// Undo is for the newest commit (HEAD) when it has a parent to go back to, its upstream,
/// if any, doesn't have it yet, and no merge, cherry-pick or revert is waiting to finish.
fn undo_offer(
    commit: &CommitInfo,
    is_newest: bool,
    unpushed: bool,
    has_upstream: bool,
    operation_unfinished: bool,
) -> UndoOffer {
    if !is_newest || commit.parents.is_empty() || operation_unfinished {
        UndoOffer::Nothing
    } else if unpushed || !has_upstream {
        UndoOffer::Undo
    } else {
        UndoOffer::PushedHint
    }
}

/// A commit's labels in the order shown: the current branch first, then local branches,
/// remote branches and tags, each alphabetically.
fn ordered_labels(labels: &[RefLabel], current_branch: Option<&str>) -> Vec<RefLabel> {
    let mut ordered = labels.to_vec();
    ordered.sort_by_key(|label| {
        let current = label.kind == LabelKind::Local && Some(label.name.as_str()) == current_branch;
        (!current, label.kind, label.name.clone())
    });
    ordered
}

/// One row of the commit graph, `lanes` wide, drawn over the row's full height.
fn graph_cell(row: &GraphRow, lanes: usize, theme: &Theme) -> AnyElement {
    let row = row.clone();
    let colors = theme.graph_lanes;
    // A merge's dot is hollow: filled with the list's own colour.
    let hole = theme.surface;
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| paint_graph_row(&row, bounds, &colors, hole, window),
    )
    .w(px(LANE_WIDTH * lanes as f32))
    .h_full()
    .flex_none()
    .into_any_element()
}

fn paint_graph_row(
    row: &GraphRow,
    bounds: Bounds<Pixels>,
    colors: &[Rgba],
    hole: Rgba,
    window: &mut Window,
) {
    let x = |lane: usize| bounds.origin.x + px(LANE_WIDTH * (lane as f32 + 0.5));
    let color = |lane: usize| colors[lane % colors.len()];
    let (top, middle, bottom) = (bounds.top(), bounds.center().y, bounds.bottom());
    let drawn = |(from, to): &(usize, usize)| from.max(to) < &MAX_LANES;
    // A line coming down into the dot bends at the dot's height; one leaving the dot bends
    // there too, then runs straight down its lane.
    for (from, to) in row.top.iter().filter(|segment| drawn(segment)) {
        let (start, end) = (point(x(*from), top), point(x(*to), middle));
        stroke(window, start, end, point(x(*from), middle), color(*from));
    }
    for (from, to) in row.bottom.iter().filter(|segment| drawn(segment)) {
        let (start, end) = (point(x(*from), middle), point(x(*to), bottom));
        stroke(window, start, end, point(x(*to), middle), color(*to));
    }
    if row.lane < MAX_LANES {
        let lane_color = color(row.lane);
        let diameter = px(DOT_RADIUS * 2.);
        let dot = Bounds::centered_at(point(x(row.lane), middle), size(diameter, diameter));
        let quad = fill(dot, if row.merge { hole } else { lane_color })
            .corner_radii(px(DOT_RADIUS))
            .border_widths(px(if row.merge { LINE_WIDTH } else { 0. }))
            .border_color(lane_color);
        window.paint_quad(quad);
    }
}

/// A line from `start` to `end`: straight within a lane, curved (around `bend`) across.
fn stroke(
    window: &mut Window,
    start: Point<Pixels>,
    end: Point<Pixels>,
    bend: Point<Pixels>,
    color: Rgba,
) {
    let mut path = PathBuilder::stroke(px(LINE_WIDTH));
    path.move_to(start);
    if start.x == end.x {
        path.line_to(end);
    } else {
        path.curve_to(end, bend);
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// "Alex Xiang" → "AX", "alex" → "A".
fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect();
    if letters.is_empty() {
        "?".to_owned()
    } else {
        letters.to_uppercase()
    }
}

/// Whether `commit` matches what the user typed: every word, in any order, in the
/// summary, the author or the hash.
fn matches_filter(commit: &CommitInfo, filter: &str) -> bool {
    let haystack = format!(
        "{} {} {}",
        commit.subject.to_lowercase(),
        commit.author.to_lowercase(),
        commit.id
    );
    filter
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

fn rows_for(commits: &[CommitInfo], filter: &str, has_more: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = commits
        .iter()
        .enumerate()
        .filter(|(_, commit)| matches_filter(commit, filter))
        .map(|(ix, _)| Row::Commit(ix))
        .collect();
    if has_more {
        rows.push(Row::LoadMore);
    }
    rows
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

impl HistoryView {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx
            .new(|cx| InputState::new(window, cx).placeholder("Filter by message, author or hash"));
        let _subscriptions = vec![
            cx.observe(&repo_state, |this, _, cx| this.reload_if_needed(cx)),
            cx.subscribe_in(&filter, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut this = Self {
            repo_state,
            filter,
            commits: Vec::new(),
            unpushed: HashSet::new(),
            scope: LogScope::Head,
            graph: Graph::default(),
            graph_rows: Vec::new(),
            labels: HashMap::new(),
            has_upstream: false,
            has_more: false,
            loaded_for: None,
            list_generation: 0,
            selected: None,
            files: Vec::new(),
            selected_file: None,
            diff: None,
            hide_whitespace: false,
            detail_generation: 0,
            diff_generation: 0,
            tasks: Vec::new(),
            _subscriptions,
        };
        this.reload_if_needed(cx);
        this
    }

    /// Reads the log again when the repository or its current commit has changed.
    fn reload_if_needed(&mut self, cx: &mut Context<Self>) {
        let state = self.repo_state.read(cx);
        let key = state.repo().zip(state.status()).map(|(repo, status)| {
            (
                repo.root().to_owned(),
                status.branch.oid.clone(),
                status.branch.ahead,
            )
        });
        if key == self.loaded_for {
            return;
        }
        self.loaded_for = key;
        self.commits.clear();
        // The current branch keeps the graph's first lane in both views.
        self.graph = match self
            .loaded_for
            .as_ref()
            .and_then(|(_, head, _)| head.as_deref())
        {
            Some(head) => Graph::reserving(head),
            None => Graph::default(),
        };
        self.graph_rows.clear();
        self.labels.clear();
        self.unpushed.clear();
        self.has_more = false;
        self.files.clear();
        self.selected_file = None;
        self.diff = None;
        self.list_generation += 1;
        self.load_page(0, cx);
        cx.notify();
    }

    fn load_page(&mut self, skip: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_state.read(cx).repo().cloned() else {
            return;
        };
        let generation = self.list_generation;
        let scope = self.scope;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let commits = repo.log(skip, PAGE, scope)?;
                    // Which commits are unpushed doesn't change as pages arrive.
                    let unpushed = if skip == 0 {
                        Some((
                            repo.unpushed_commits()?,
                            repo.has_upstream(),
                            repo.ref_labels()?,
                        ))
                    } else {
                        None
                    };
                    Ok::<_, git_core::Error>((commits, unpushed))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.list_generation != generation {
                    return;
                }
                match result {
                    Ok((commits, unpushed)) => {
                        this.has_more = commits.len() == PAGE;
                        for commit in &commits {
                            let row = this.graph.push(&commit.id, &commit.parents);
                            this.graph_rows.push(row);
                        }
                        this.commits.extend(commits);
                        if let Some((unpushed, has_upstream, labels)) = unpushed {
                            this.unpushed = unpushed;
                            this.has_upstream = has_upstream;
                            this.labels = labels;
                        }
                        // Keep the selection if it is still there, otherwise show the newest.
                        let still_there = this
                            .selected
                            .as_ref()
                            .is_some_and(|id| this.commits.iter().any(|c| &c.id == id));
                        if !still_there {
                            if let Some(first) = this.commits.first().map(|c| c.id.clone()) {
                                this.select_commit(first, cx);
                            } else {
                                this.selected = None;
                            }
                        } else if this.files.is_empty() {
                            // The reload cleared the detail; bring it back for the same commit.
                            this.load_files(cx);
                        }
                    }
                    Err(error) => this.repo_state.update(cx, |state, cx| {
                        state.report(format!("Couldn't read the history: {error}"), cx)
                    }),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn selected_commit(&self) -> Option<&CommitInfo> {
        let id = self.selected.as_ref()?;
        self.commits.iter().find(|commit| &commit.id == id)
    }

    fn select_commit(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(id.as_str()) && !self.files.is_empty() {
            return;
        }
        self.selected = Some(id);
        self.load_files(cx);
    }

    /// Reads the files of the selected commit and shows the first one's diff.
    fn load_files(&mut self, cx: &mut Context<Self>) {
        self.files.clear();
        self.selected_file = None;
        self.diff = None;
        self.detail_generation += 1;
        let (Some(repo), Some(commit)) = (
            self.repo_state.read(cx).repo().cloned(),
            self.selected_commit().cloned(),
        ) else {
            return;
        };
        let generation = self.detail_generation;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.commit_files(&commit) })
                .await;
            this.update(cx, |this, cx| {
                if this.detail_generation != generation {
                    return;
                }
                match result {
                    Ok(files) => {
                        this.files = files;
                        if !this.files.is_empty() {
                            this.select_file(0, cx);
                        }
                    }
                    Err(error) => this.diff = Some(DiffContent::Failed(error.to_string().into())),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select_file(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_file = Some(index);
        self.diff = None;
        self.diff_generation += 1;
        self.load_diff(cx);
        cx.notify();
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(commit), Some(file)) = (
            self.repo_state.read(cx).repo().cloned(),
            self.selected_commit().cloned(),
            self.selected_file
                .and_then(|ix| self.files.get(ix).cloned()),
        ) else {
            return;
        };
        let options = DiffOptions {
            ignore_whitespace: self.hide_whitespace,
        };
        let generation = self.diff_generation;
        let path = file.path.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repo.commit_diff(&commit, &file, options) })
                .await;
            this.update(cx, |this, cx| {
                if this.diff_generation != generation {
                    return;
                }
                this.diff = Some(match result {
                    Ok(diff) => DiffContent::from_diffs(
                        &FileDiffs {
                            staged: git_core::FileDiff::default(),
                            unstaged: diff,
                        },
                        // History is read-only: no stage or unstage buttons.
                        HunkControls::default(),
                        &path,
                    ),
                    Err(error) => DiffContent::Failed(error.to_string().into()),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// "This branch | All branches": which commits the list and its graph show.
    fn scope_switch(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let option = |id: &'static str, label: &'static str, scope: LogScope| {
            let picked = self.scope == scope;
            div()
                .id(id)
                .flex_1()
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme.metrics.radius_md))
                .text_size(px(theme.metrics.text_sm))
                .cursor_pointer()
                .text_color(if picked { theme.text } else { theme.text_muted })
                .when(picked, |option| option.bg(theme.pill_active))
                .on_click(cx.listener(move |this, _, _, cx| this.set_scope(scope, cx)))
                .child(label)
        };
        div()
            .flex()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(theme.metrics.radius_md))
            .border_1()
            .border_color(theme.border)
            .child(option("scope-head", "This branch", LogScope::Head))
            .child(option("scope-all", "All branches", LogScope::All))
            .into_any_element()
    }

    /// Shows the current branch's commits or every branch's, reading the list again.
    fn set_scope(&mut self, scope: LogScope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.loaded_for = None;
        self.reload_if_needed(cx);
    }

    fn toggle_whitespace(&mut self, cx: &mut Context<Self>) {
        self.hide_whitespace = !self.hide_whitespace;
        self.diff = None;
        self.diff_generation += 1;
        self.load_diff(cx);
        cx.notify();
    }

    /// One commit in the list. `graph` is its graph row and the column's width in lanes,
    /// or `None` to show the author's initials instead (while filtering).
    fn commit_row(
        &self,
        ix: usize,
        commit: &CommitInfo,
        graph: Option<(&GraphRow, usize)>,
        current_branch: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected.as_deref() == Some(commit.id.as_str());
        let unpushed = self.unpushed.contains(&commit.id);
        let view = cx.entity();
        let id = commit.id.clone();
        let labels = self
            .labels
            .get(&commit.id)
            .map(|labels| ordered_labels(labels, current_branch))
            .unwrap_or_default();
        let hidden_labels = labels.len().saturating_sub(MAX_LABELS);
        let avatar = div()
            .size(px(30.))
            .flex_none()
            .rounded_full()
            .bg(theme.pill_active)
            .text_size(px(theme.metrics.text_xs))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .flex()
            .items_center()
            .justify_center()
            .child(initials(&commit.author));
        div()
            .id(("commit", ix))
            .w_full()
            .h(px(COMMIT_ROW_HEIGHT))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme.surface_selected))
            .hover(|row| row.bg(theme.surface_selected))
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.select_commit(id.clone(), cx))
            })
            .child(match graph {
                Some((row, lanes)) => graph_cell(row, lanes, theme),
                None => avatar.into_any_element(),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(SharedString::from(commit.subject.clone())),
                            )
                            .children(labels.into_iter().take(MAX_LABELS).map(|label| {
                                let color = match label.kind {
                                    LabelKind::Local => theme.accent_soft,
                                    LabelKind::Remote => theme.text_muted,
                                    LabelKind::Tag => theme.syntax_type,
                                };
                                div()
                                    .flex_none()
                                    .max_w(px(120.))
                                    .truncate()
                                    .px(px(6.))
                                    .rounded(px(theme.metrics.radius_sm))
                                    .bg(theme.pill_active)
                                    .text_size(px(theme.metrics.text_xs))
                                    .font_family(MONO_FONT)
                                    .text_color(color)
                                    .child(SharedString::from(label.name))
                            }))
                            .when(hidden_labels > 0, |line| {
                                line.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(theme.metrics.text_xs))
                                        .text_color(theme.text_muted)
                                        .child(format!("+{hidden_labels}")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(theme.metrics.text_sm))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "{} · {}",
                                commit.author,
                                relative_time(commit.time, now())
                            )),
                    ),
            )
            .when(unpushed, |row| {
                row.child(
                    div()
                        .size(px(8.))
                        .flex_none()
                        .rounded(px(theme.metrics.radius_sm))
                        .bg(theme.modified.text),
                )
            })
            .child(
                div()
                    .flex_none()
                    .font_family(MONO_FONT)
                    .text_size(px(theme.metrics.text_xs))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(commit.short_id.clone())),
            )
            .into_any_element()
    }

    fn detail(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(commit) = self.selected_commit().cloned() else {
            return message(theme, "Select a commit to see what it changed");
        };
        let button = |id: &'static str| {
            div()
                .id(id)
                .h(px(theme.metrics.control_height))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|button| button.bg(theme.surface_selected))
        };
        let (revert, pick, undo) = (commit.clone(), commit.clone(), commit.id.clone());
        let is_newest = self
            .loaded_for
            .as_ref()
            .and_then(|(_, head, _)| head.as_deref())
            == Some(commit.id.as_str());
        let offer = undo_offer(
            &commit,
            is_newest,
            self.unpushed.contains(&commit.id),
            self.has_upstream,
            self.repo_state.read(cx).in_progress().is_some(),
        );

        let header = div()
            .flex_none()
            .px(px(20.))
            .py(px(16.))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(theme.metrics.text_lg))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(commit.subject.clone())),
            )
            .when(!commit.body.is_empty(), |column| {
                column.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(commit.body.clone())),
                )
            })
            .child(
                div()
                    .mt(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .text_size(px(theme.metrics.text_sm))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "{} · {}",
                        commit.author,
                        relative_time(commit.time, now())
                    ))
                    .child(
                        div()
                            .px(px(8.))
                            .rounded(px(theme.metrics.radius_md))
                            .bg(theme.pill_active)
                            .text_color(theme.text)
                            .font_family(MONO_FONT)
                            .child(SharedString::from(commit.short_id.clone())),
                    )
                    .children(commit.parents.first().map(|parent| {
                        div().child(format!("Parent {}", &parent[..parent.len().min(7)]))
                    }))
                    .child(div().flex_1())
                    .when(offer == UndoOffer::PushedHint, |row| {
                        row.child("Pushed · Revert to undo it")
                    })
                    .when(offer == UndoOffer::Undo, |row| {
                        row.child(
                            button("undo")
                                .text_color(theme.text)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let shown = undo.clone();
                                    this.repo_state
                                        .update(cx, |state, cx| state.undo_last_commit(shown, cx));
                                }))
                                .child("Undo"),
                        )
                    })
                    .child(
                        button("revert")
                            .text_color(theme.text)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let commit = revert.clone();
                                this.repo_state
                                    .update(cx, |state, cx| state.revert_commit(commit, cx));
                            }))
                            .child("Revert"),
                    )
                    .child(
                        button("cherry-pick")
                            .text_color(theme.text)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let commit = pick.clone();
                                this.repo_state
                                    .update(cx, |state, cx| state.cherry_pick_commit(commit, cx));
                            }))
                            .child("Cherry-pick"),
                    ),
            );

        let file_rows: Vec<AnyElement> = self
            .files
            .iter()
            .enumerate()
            .map(|(ix, file)| {
                let selected = self.selected_file == Some(ix);
                let name = file
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file.path)
                    .to_owned();
                div()
                    .id(("commit-file", ix))
                    .w_full()
                    .h(px(theme.metrics.row_height))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.surface_selected))
                    .hover(|row| row.bg(theme.surface_selected))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_file(ix, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    )
                    .children(file.additions.map(|n| {
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(theme.metrics.text_xs))
                            .text_color(theme.added.text)
                            .child(format!("+{n}"))
                    }))
                    .children(file.deletions.map(|n| {
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(theme.metrics.text_xs))
                            .text_color(theme.deleted.text)
                            .child(format!("−{n}"))
                    }))
                    .into_any_element()
            })
            .collect();

        let hide_whitespace = self.hide_whitespace;
        let body = diff_body(
            self.diff.as_ref(),
            ("history-diff", self.diff_generation as usize),
            hide_whitespace,
            theme,
            DiffInteraction::read_only(),
        );

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(260.))
                            .flex_none()
                            .border_r_1()
                            .border_color(theme.border)
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .h(px(theme.metrics.row_height))
                                    .flex_none()
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .text_color(theme.text_muted)
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(match self.files.len() {
                                        1 => "1 changed file".to_owned(),
                                        n => format!("{n} changed files"),
                                    }),
                            )
                            .children(file_rows),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .h(px(theme.metrics.row_height))
                                    .flex_none()
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .id("history-whitespace")
                                            .px(px(10.))
                                            .h(px(26.))
                                            .flex()
                                            .items_center()
                                            .rounded(px(theme.metrics.radius_md))
                                            .cursor_pointer()
                                            .text_size(px(theme.metrics.text_sm))
                                            .text_color(if hide_whitespace {
                                                theme.accent_soft
                                            } else {
                                                theme.text_muted
                                            })
                                            .hover(|button| button.bg(theme.surface_selected))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_whitespace(cx)
                                            }))
                                            .child(if hide_whitespace {
                                                "Whitespace hidden"
                                            } else {
                                                "Hide whitespace"
                                            }),
                                    ),
                            )
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let filter = self.filter.read(cx).value().to_string();
        let rows = rows_for(&self.commits, &filter, self.has_more);
        // Filtered rows would break the graph's lines, so it shows only for the full list.
        let lanes = filter.trim().is_empty().then(|| {
            self.graph_rows
                .iter()
                .map(GraphRow::width)
                .max()
                .unwrap_or(1)
                .clamp(1, MAX_LANES)
        });
        let current_branch = match self.repo_state.read(cx).status().map(|s| &s.branch.head) {
            Some(Head::Branch(name)) => Some(name.clone()),
            _ => None,
        };

        let list: AnyElement = if rows.is_empty() {
            message(
                &theme,
                if self.loaded_for.is_none() {
                    "No repository open"
                } else if self.commits.is_empty() && filter.trim().is_empty() {
                    "No commits yet"
                } else {
                    "No commits match"
                },
            )
        } else {
            let row_theme = theme.clone();
            let loaded = self.commits.len();
            uniform_list(
                "commits",
                rows.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|ix| match rows.get(ix)? {
                            Row::Commit(commit_ix) => {
                                let commit = this.commits.get(*commit_ix)?.clone();
                                let graph_row = this.graph_rows.get(*commit_ix).cloned();
                                let graph = lanes.zip(graph_row.as_ref()).map(|(w, r)| (r, w));
                                Some(this.commit_row(
                                    ix,
                                    &commit,
                                    graph,
                                    current_branch.as_deref(),
                                    &row_theme,
                                    cx,
                                ))
                            }
                            Row::LoadMore => {
                                let view = cx.entity();
                                Some(
                                    div()
                                        .id("load-more")
                                        .w_full()
                                        .h(px(COMMIT_ROW_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(row_theme.accent_soft)
                                        .hover(|row| row.bg(row_theme.surface_selected))
                                        .on_click(move |_, _, cx| {
                                            view.update(cx, |this, cx| this.load_page(loaded, cx))
                                        })
                                        .child("Load more commits…")
                                        .into_any_element(),
                                )
                            }
                        })
                        .collect()
                }),
            )
            .flex_1()
            .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(12.))
            .px(px(12.))
            .pb(px(12.))
            .child(
                panel(&theme)
                    .w(px(360.))
                    .flex_none()
                    .child(
                        div()
                            .flex_none()
                            .p(px(10.))
                            .border_b_1()
                            .border_color(theme.border)
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(Input::new(&self.filter).aria_label("Filter commits"))
                            .child(self.scope_switch(&theme, cx)),
                    )
                    .child(list),
            )
            .child(
                panel(&theme)
                    .flex_1()
                    .min_w_0()
                    .child(self.detail(&theme, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str, author: &str) -> CommitInfo {
        CommitInfo {
            id: "abcdef0123456789".into(),
            short_id: "abcdef0".into(),
            author: author.into(),
            email: "a@x.io".into(),
            time: 0,
            parents: Vec::new(),
            subject: subject.into(),
            body: String::new(),
        }
    }

    fn with_parent(mut commit: CommitInfo) -> CommitInfo {
        commit.parents = vec!["0123456".into()];
        commit
    }

    #[test]
    fn the_newest_unpushed_commit_can_be_undone() {
        let c = with_parent(commit("Add b", "Sam"));
        assert_eq!(undo_offer(&c, true, true, true, false), UndoOffer::Undo);
        // No upstream: nothing is pushed.
        assert_eq!(undo_offer(&c, true, false, false, false), UndoOffer::Undo);
    }

    #[test]
    fn no_undo_while_a_merge_or_pick_is_unfinished() {
        let c = with_parent(commit("Add b", "Sam"));
        assert_eq!(undo_offer(&c, true, true, true, true), UndoOffer::Nothing);
    }

    #[test]
    fn a_pushed_newest_commit_points_at_revert() {
        let c = with_parent(commit("Add b", "Sam"));
        assert_eq!(
            undo_offer(&c, true, false, true, false),
            UndoOffer::PushedHint
        );
    }

    #[test]
    fn older_commits_and_the_first_commit_offer_no_undo() {
        let c = with_parent(commit("Add b", "Sam"));
        assert_eq!(undo_offer(&c, false, true, true, false), UndoOffer::Nothing);
        assert_eq!(
            undo_offer(&commit("base", "Sam"), true, true, false, false),
            UndoOffer::Nothing
        );
    }

    fn label(kind: LabelKind, name: &str) -> RefLabel {
        RefLabel {
            kind,
            name: name.into(),
        }
    }

    #[test]
    fn the_current_branch_label_comes_first_then_branches_remotes_and_tags() {
        let labels = [
            label(LabelKind::Tag, "v1"),
            label(LabelKind::Remote, "origin/main"),
            label(LabelKind::Local, "zeta"),
            label(LabelKind::Local, "main"),
        ];

        let ordered = ordered_labels(&labels, Some("zeta"));

        let names: Vec<_> = ordered.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["zeta", "main", "origin/main", "v1"]);
    }

    #[test]
    fn initials_come_from_the_first_two_words() {
        assert_eq!(initials("Alex Xiang"), "AX");
        assert_eq!(initials("alex"), "A");
        assert_eq!(initials("Mary Jane Watson"), "MJ");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn the_filter_looks_in_subject_author_and_hash_in_any_order() {
        let c = commit("Fix status polling", "Sam Lee");
        assert!(matches_filter(&c, "polling"));
        assert!(matches_filter(&c, "sam"));
        assert!(matches_filter(&c, "ABCDEF0"));
        assert!(matches_filter(&c, "status SAM"));
        assert!(matches_filter(&c, ""));
        assert!(!matches_filter(&c, "polling alex"));
    }

    #[test]
    fn a_load_more_row_follows_the_commits_only_when_there_are_more() {
        let commits = vec![commit("one", "A"), commit("two", "B")];
        assert_eq!(
            rows_for(&commits, "", false),
            [Row::Commit(0), Row::Commit(1)]
        );
        assert_eq!(
            rows_for(&commits, "", true),
            [Row::Commit(0), Row::Commit(1), Row::LoadMore]
        );
    }

    #[test]
    fn filtering_keeps_the_original_indexes_of_the_matches() {
        let commits = vec![
            commit("alpha", "A"),
            commit("beta", "B"),
            commit("alphabet", "C"),
        ];
        assert_eq!(
            rows_for(&commits, "alpha", false),
            [Row::Commit(0), Row::Commit(2)]
        );
    }
}
