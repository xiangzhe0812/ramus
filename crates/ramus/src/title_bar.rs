use git_core::{Head, RepoStatus};
use gpui::{
    App, Context, Entity, EventEmitter, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription,
    Window, div, prelude::FluentBuilder, px,
};

use crate::{
    actions::{
        AddRemote, NewBranchHere, ToggleBranches, TogglePalette, ToggleRepositories,
        ToggleWorktrees,
    },
    pull_requests::{Checks, PrLink, PullRequestState},
    repo_state::{RepoState, SyncKind},
    theme::{MONO_FONT, TITLE_BAR_HEIGHT, Theme},
    ui::icon,
    update_check::{UpdateCheck, release_page},
};

/// The views the window can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Changes,
    History,
    Stashes,
    Worktrees,
}

/// The user clicked a tab.
pub struct TabRequested(pub Tab);

impl EventEmitter<TabRequested> for TitleBar {}

pub struct TitleBar {
    repo_state: Entity<RepoState>,
    update_check: Entity<UpdateCheck>,
    pull_requests: Entity<PullRequestState>,
    tab: Tab,
    _observe: Vec<Subscription>,
}

impl TitleBar {
    pub fn new(
        repo_state: Entity<RepoState>,
        update_check: Entity<UpdateCheck>,
        pull_requests: Entity<PullRequestState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _observe = vec![
            cx.observe(&repo_state, |_, _, cx| cx.notify()),
            cx.observe(&update_check, |_, _, cx| cx.notify()),
            cx.observe(&pull_requests, |_, _, cx| cx.notify()),
        ];
        Self {
            repo_state,
            update_check,
            pull_requests,
            tab: Tab::Changes,
            _observe,
        }
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }
}

/// What the main button does for this branch, its label, and a count to badge it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncAction {
    pub kind: SyncKind,
    pub label: String,
    pub count: Option<u32>,
    /// The remote it talks to, for sentences like "Push 2 commits to origin".
    pub remote: String,
}

/// The one thing most worth doing next: publish a branch that has no upstream, pull what
/// is waiting, push what is ready, and otherwise look for news.
pub fn primary_sync(status: &RepoStatus) -> Option<SyncAction> {
    let branch = &status.branch;
    // Nothing to exchange until there is a first commit.
    branch.oid.as_ref()?;
    let remote = branch
        .upstream
        .as_deref()
        .and_then(|upstream| upstream.split('/').next())
        .unwrap_or("origin");
    let action = |kind, verb: &str, count| SyncAction {
        kind,
        label: format!("{verb} {remote}"),
        count,
        remote: remote.to_owned(),
    };
    Some(
        if branch.upstream.is_none() && matches!(branch.head, Head::Branch(_)) {
            SyncAction {
                kind: SyncKind::Publish,
                label: "Publish branch".into(),
                count: None,
                remote: remote.to_owned(),
            }
        } else if branch.behind > 0 {
            action(SyncKind::Pull, "Pull", Some(branch.behind))
        } else if branch.ahead > 0 {
            action(SyncKind::Push, "Push", Some(branch.ahead))
        } else {
            action(SyncKind::Fetch, "Fetch", None)
        },
    )
}

/// Space between the window's edge and the first pill.
const BAR_PADDING: f32 = 16.;
/// Room on the left for the window's close, minimise and zoom buttons, which sit inside
/// the bar now that the window has no native title bar.
const TRAFFIC_LIGHTS_WIDTH: f32 = 64.;
/// Where the traffic lights sit: inset from the left, centred on the bar.
pub const TRAFFIC_LIGHTS_POSITION: (f32, f32) = (16., 14.);
const PILL_GAP: f32 = 8.;
/// The repository and worktree pills are exactly this wide, so the pills after them, and
/// the lists that hang under those, stay put however long a name is. The branch pill is
/// at least this wide.
const PILL_WIDTH: f32 = 200.;
/// Where a popover hanging under a title-bar pill starts: just below the bar.
pub const POPOVER_TOP: f32 = TITLE_BAR_HEIGHT + 4.;
/// Where the repository pill starts, for the list that hangs under it.
pub const REPO_PILL_LEFT: f32 = BAR_PADDING + TRAFFIC_LIGHTS_WIDTH;
/// Where the worktree pill starts, for the list that hangs under it.
pub const WORKTREE_PILL_LEFT: f32 = REPO_PILL_LEFT + PILL_WIDTH + PILL_GAP;
/// Where the branch pill starts, for the list that hangs under it.
pub const BRANCH_PILL_LEFT: f32 = WORKTREE_PILL_LEFT + PILL_WIDTH + PILL_GAP;

fn pill(theme: &Theme) -> gpui::Div {
    div()
        .h(px(theme.metrics.control_height))
        .min_w(px(PILL_WIDTH))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(theme.metrics.radius_md))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .text_color(theme.text)
}

/// Keeps a press on a button in the bar from also starting a window drag.
fn keep_click(_: &MouseDownEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

impl Render for TitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let state = self.repo_state.read(cx);
        let repo_name = state
            .repository_name()
            .unwrap_or_else(|| "No repository".into());
        // The open worktree's folder name. The pill shows whenever a repository is open.
        let worktree_name = state.repo().map(|repo| repo.name());
        let branch = state.status().map(|status| &status.branch);
        let update = self.update_check.read(cx).available();
        let pull = match self.pull_requests.read(cx).link() {
            Some(PrLink::Open {
                number,
                checks,
                url,
            }) => Some((number, checks, url)),
            _ => None,
        };

        let branch_label = branch.map(|branch| match &branch.head {
            Head::Branch(name) if branch.oid.is_none() => format!("{name} (no commits)"),
            Head::Branch(name) => name.clone(),
            Head::Detached(sha) => format!("detached at {sha}"),
        });
        let local_only = state.repo().is_some() && !state.has_remote();
        let has_commits = branch.is_some_and(|branch| branch.oid.is_some());
        let detached = branch.is_some_and(|branch| matches!(branch.head, Head::Detached(_)));
        let running = state.operation().is_some();
        let sync = state
            .status()
            .and_then(primary_sync)
            .filter(|_| state.has_remote());
        let operation = state.operation().map(|operation| {
            let verb = if operation.cancelling {
                "Cancelling".to_owned()
            } else {
                operation.kind.progressive().to_owned()
            };
            match &operation.progress {
                Some(progress) if !operation.cancelling => {
                    format!("{verb}… {}%", progress.percent)
                }
                _ => format!("{verb}…"),
            }
        });

        div()
            .h(px(TITLE_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(PILL_GAP))
            .pl(px(REPO_PILL_LEFT))
            .pr(px(BAR_PADDING))
            // Empty space in the bar moves the window, and a double-click zooms it, as a
            // native title bar would. Buttons in the bar stop the press with `keep_click`.
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                pill(&theme)
                    .id("repo-pill")
                    .max_w(px(PILL_WIDTH))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .hover(|style| style.bg(theme.surface_selected))
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(ToggleRepositories), cx)
                    }))
                    .child(icon("repository", 16., theme.text_muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(repo_name),
                    )
                    .when(local_only, |pill| {
                        pill.child(
                            div()
                                .flex_none()
                                .px(px(8.))
                                .py(px(2.))
                                .rounded(px(theme.metrics.radius_md))
                                .bg(theme.pill_active)
                                .text_size(px(theme.metrics.text_xs))
                                .text_color(theme.text_muted)
                                .child("Local only"),
                        )
                    }),
            )
            .children(worktree_name.map(|name| {
                pill(&theme)
                    .id("worktree-pill")
                    .max_w(px(PILL_WIDTH))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .hover(|style| style.bg(theme.surface_selected))
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(ToggleWorktrees), cx)
                    }))
                    .child(icon("worktree", 16., theme.text_muted))
                    .child(div().flex_1().min_w_0().truncate().child(name))
            }))
            .children(branch_label.map(|label| {
                let pill = pill(&theme)
                    .id("branch-pill")
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .hover(|style| style.bg(theme.surface_selected))
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(ToggleBranches), cx)
                    }))
                    .font_family(MONO_FONT);
                if detached {
                    pill.bg(theme.modified.background)
                        .text_color(theme.modified.text)
                } else {
                    pill
                }
                .child(icon("branch", 16., theme.text_muted))
                .child(div().flex_1().min_w_0().truncate().child(label))
            }))
            .children(pull.map(|(number, checks, url)| {
                // The branch's open pull request and where its checks stand; opens it.
                let mark = match checks {
                    Checks::Passing => {
                        Some(icon("check", 12., theme.diff_added_sign).into_any_element())
                    }
                    Checks::Failing => {
                        Some(icon("close", 12., theme.deleted.text).into_any_element())
                    }
                    Checks::Running => Some(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(theme.modified.text)
                            .into_any_element(),
                    ),
                    Checks::None => None,
                };
                div()
                    .id("pull-request")
                    .h(px(theme.metrics.control_height))
                    .px(px(10.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(theme.metrics.radius_md))
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|pill| pill.bg(theme.surface_selected))
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(format!("#{number}"))
                    .children(mark)
            }))
            .child(div().flex_1())
            .children(operation.map(|label| {
                // While something runs, the button shows it, and clicking it cancels.
                div()
                    .id("sync-progress")
                    .h(px(theme.metrics.control_height))
                    .min_w(px(200.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(theme.metrics.radius_md))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.pill_active)
                    .text_color(theme.text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.repo_state
                            .update(cx, |state, cx| state.cancel_sync(cx))
                    }))
                    .child(div().flex_1().child(label))
                    .child(
                        div()
                            .text_size(px(theme.metrics.text_xs))
                            .font_weight(gpui::FontWeight::NORMAL)
                            .text_color(theme.text_muted)
                            .child("Cancel"),
                    )
            }))
            .children(sync.filter(|_| !running && !detached).map(|action| {
                let kind = action.kind;
                div()
                    .id("sync")
                    .h(px(theme.metrics.control_height))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(theme.metrics.radius_md))
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.repo_state.update(cx, |state, cx| state.sync(kind, cx))
                    }))
                    .child(action.label)
                    .children(action.count.map(|count| {
                        div()
                            .px(px(7.))
                            .rounded(px(theme.metrics.radius_md))
                            .text_size(px(theme.metrics.text_xs))
                            .bg(theme.accent_chip)
                            .child(count.to_string())
                    }))
            }))
            .when(detached && !running, |bar| {
                // New commits here would be unreachable once HEAD moves on.
                bar.child(
                    div()
                        .id("branch-here")
                        .h(px(theme.metrics.control_height))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(theme.metrics.radius_md))
                        .bg(theme.accent)
                        .text_color(theme.accent_text)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, keep_click)
                        .on_click(cx.listener(|_, _, window, cx| {
                            window.dispatch_action(Box::new(NewBranchHere), cx)
                        }))
                        .child("Create branch here…"),
                )
            })
            .when(local_only && has_commits && !running, |bar| {
                bar.child(
                    div()
                        .id("add-remote")
                        .h(px(theme.metrics.control_height))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(theme.metrics.radius_md))
                        // The same as the sync button, whose place it takes in a local-only
                        // repository.
                        .bg(theme.accent)
                        .text_color(theme.accent_text)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, keep_click)
                        .on_click(cx.listener(|_, _, window, cx| {
                            window.dispatch_action(Box::new(AddRemote), cx)
                        }))
                        .child("Add remote…"),
                )
            })
            .children(update.map(|version| {
                // Opens the release's page; installing stays in the user's hands.
                div()
                    .id("update")
                    .h(px(theme.metrics.control_height))
                    .px(px(12.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(px(theme.metrics.radius_md))
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .on_click(move |_, _, cx| cx.open_url(&release_page(version)))
                    .child(format!("Update · {version}"))
            }))
            .child(
                div()
                    .id("palette-button")
                    .h(px(theme.metrics.control_height))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(theme.metrics.radius_md))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, keep_click)
                    .hover(|b| b.bg(theme.surface_selected))
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(TogglePalette), cx)
                    }))
                    .child(icon("search", 14., theme.text_muted))
                    .child("Search")
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(theme.metrics.text_xs))
                            .child("⌘ K"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .p(px(3.))
                    .gap(px(2.))
                    .rounded(px(theme.metrics.radius_md))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .children(
                        [
                            (Tab::Changes, "Changes"),
                            (Tab::History, "History"),
                            (Tab::Stashes, "Stashes"),
                            (Tab::Worktrees, "Worktrees"),
                        ]
                        .map(|(tab, label)| {
                            let active = self.tab == tab;
                            div()
                                .id(label)
                                .h(px(theme.metrics.control_height - 8.))
                                .px(px(12.))
                                .flex()
                                .items_center()
                                .rounded(px(theme.metrics.radius_sm))
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, keep_click)
                                .when(active, |pill| {
                                    pill.bg(theme.pill_active)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .when(!active, |pill| {
                                    pill.text_color(theme.text_muted)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .hover(|pill| pill.bg(theme.surface_selected))
                                })
                                .on_click(
                                    cx.listener(move |_, _, _, cx| cx.emit(TabRequested(tab))),
                                )
                                .child(label)
                        }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use git_core::BranchStatus;

    use super::*;

    fn status(
        upstream: Option<&str>,
        ahead: u32,
        behind: u32,
        has_commits: bool,
        head: Head,
    ) -> RepoStatus {
        RepoStatus {
            branch: BranchStatus {
                head,
                oid: has_commits.then(|| "a".repeat(40)),
                upstream: upstream.map(str::to_owned),
                ahead,
                behind,
            },
            files: Vec::new(),
        }
    }

    fn main_branch() -> Head {
        Head::Branch("main".into())
    }

    fn action(status: &RepoStatus) -> (SyncKind, String, Option<u32>) {
        let action = primary_sync(status).expect("an action");
        (action.kind, action.label, action.count)
    }

    #[test]
    fn a_branch_with_no_upstream_is_published() {
        let s = status(None, 0, 0, true, main_branch());
        assert_eq!(
            action(&s),
            (SyncKind::Publish, "Publish branch".into(), None)
        );
    }

    #[test]
    fn commits_waiting_to_go_out_make_the_button_push() {
        let s = status(Some("origin/main"), 2, 0, true, main_branch());
        assert_eq!(action(&s), (SyncKind::Push, "Push origin".into(), Some(2)));
    }

    #[test]
    fn commits_waiting_to_come_in_make_the_button_pull() {
        let s = status(Some("upstream/main"), 0, 3, true, main_branch());
        assert_eq!(
            action(&s),
            (SyncKind::Pull, "Pull upstream".into(), Some(3))
        );
    }

    #[test]
    fn a_diverged_branch_offers_to_pull_first() {
        let s = status(Some("origin/main"), 1, 1, true, main_branch());
        assert_eq!(action(&s).0, SyncKind::Pull);
    }

    #[test]
    fn an_up_to_date_branch_offers_a_fetch() {
        let s = status(Some("origin/main"), 0, 0, true, main_branch());
        assert_eq!(action(&s), (SyncKind::Fetch, "Fetch origin".into(), None));
    }

    #[test]
    fn a_branch_with_no_commits_has_nothing_to_exchange() {
        assert_eq!(
            primary_sync(&status(None, 0, 0, false, main_branch())),
            None
        );
    }

    #[test]
    fn a_detached_head_can_fetch_but_not_publish() {
        let s = status(None, 0, 0, true, Head::Detached("abc1234".into()));
        assert_eq!(action(&s).0, SyncKind::Fetch);
    }
}
