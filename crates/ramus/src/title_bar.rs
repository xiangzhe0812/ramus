use git_core::{Head, RepoStatus};
use gpui::{
    Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    actions::{OpenRepository, ToggleBranches},
    repo_state::{RepoState, SyncKind},
    theme::{MONO_FONT, Theme},
};

/// The two views the window can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Changes,
    History,
}

/// The user clicked a tab.
pub struct TabRequested(pub Tab);

impl EventEmitter<TabRequested> for TitleBar {}

pub struct TitleBar {
    repo_state: Entity<RepoState>,
    tab: Tab,
    _observe: Subscription,
}

impl TitleBar {
    pub fn new(repo_state: Entity<RepoState>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&repo_state, |_, _, cx| cx.notify());
        Self {
            repo_state,
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
    };
    Some(
        if branch.upstream.is_none() && matches!(branch.head, Head::Branch(_)) {
            SyncAction {
                kind: SyncKind::Publish,
                label: "Publish branch".into(),
                count: None,
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

fn pill(theme: &Theme) -> gpui::Div {
    div()
        .h(px(40.))
        .min_w(px(220.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(20.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .text_color(theme.text)
}

impl Render for TitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let state = self.repo_state.read(cx);
        let repo_name = state
            .repo()
            .map(|repo| repo.name())
            .unwrap_or_else(|| "No repository".into());
        let branch = state.status().map(|status| &status.branch);

        let branch_label = branch.map(|branch| match &branch.head {
            Head::Branch(name) if branch.oid.is_none() => format!("{name} (no commits)"),
            Head::Branch(name) => name.clone(),
            Head::Detached(sha) => format!("detached at {sha}"),
        });
        let detached = branch.is_some_and(|branch| matches!(branch.head, Head::Detached(_)));
        let running = state.operation().is_some();
        let sync = state.status().and_then(primary_sync);
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
            .h(px(60.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .child(
                pill(&theme)
                    .id("repo-pill")
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_selected))
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(OpenRepository), cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(repo_name),
                    ),
            )
            .children(branch_label.map(|label| {
                let pill = pill(&theme)
                    .id("branch-pill")
                    .cursor_pointer()
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
                .child(div().flex_1().child(label))
            }))
            .children(operation.map(|label| {
                // While something runs, the button shows it, and clicking it cancels.
                div()
                    .id("sync-progress")
                    .h(px(40.))
                    .min_w(px(200.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(20.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.pill_active)
                    .text_color(theme.text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.repo_state
                            .update(cx, |state, cx| state.cancel_sync(cx))
                    }))
                    .child(div().flex_1().child(label))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::NORMAL)
                            .text_color(theme.text_muted)
                            .child("Cancel"),
                    )
            }))
            .children(sync.filter(|_| !running).map(|action| {
                let kind = action.kind;
                div()
                    .id("sync")
                    .h(px(40.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(20.))
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.repo_state.update(cx, |state, cx| state.sync(kind, cx))
                    }))
                    .child(action.label)
                    .children(action.count.map(|count| {
                        div()
                            .px(px(7.))
                            .rounded(px(10.))
                            .text_size(px(11.))
                            .bg(gpui::rgba(0xffffff38))
                            .child(count.to_string())
                    }))
            }))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .p(px(3.))
                    .gap(px(2.))
                    .rounded(px(20.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .children([(Tab::Changes, "Changes"), (Tab::History, "History")].map(
                        |(tab, label)| {
                            let active = self.tab == tab;
                            div()
                                .id(label)
                                .h(px(32.))
                                .px(px(16.))
                                .flex()
                                .items_center()
                                .rounded(px(16.))
                                .cursor_pointer()
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
                        },
                    )),
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
