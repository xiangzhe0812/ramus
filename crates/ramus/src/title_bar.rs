use git_core::Head;
use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};

use crate::{
    actions::{OpenRepository, ToggleBranches},
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
};

pub struct TitleBar {
    repo_state: Entity<RepoState>,
    _observe: Subscription,
}

impl TitleBar {
    pub fn new(repo_state: Entity<RepoState>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&repo_state, |_, _, cx| cx.notify());
        Self {
            repo_state,
            _observe,
        }
    }
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
        let push_label = branch.and_then(|branch| {
            let remote = branch.upstream.as_deref()?.split('/').next()?;
            (branch.ahead > 0).then(|| (format!("Push {remote}"), branch.ahead))
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
            .children(push_label.map(|(label, ahead)| {
                div()
                    .h(px(40.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(20.))
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(label)
                    .child(
                        div()
                            .px(px(7.))
                            .rounded(px(10.))
                            .text_size(px(11.))
                            .bg(gpui::rgba(0xffffff38))
                            .child(ahead.to_string()),
                    )
            }))
    }
}
