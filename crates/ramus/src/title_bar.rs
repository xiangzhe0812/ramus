use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, px};

use crate::theme::Theme;

/// Static placeholder content for milestone 1; real repo state arrives with `RepoState`.
pub struct TitleBar;

impl TitleBar {
    fn pill(theme: &Theme, label: &'static str) -> gpui::Div {
        div()
            .h(px(40.))
            .min_w(px(220.))
            .px(px(14.))
            .flex()
            .items_center()
            .rounded(px(20.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .text_color(theme.text)
            .child(div().flex_1().child(label))
    }
}

impl Render for TitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        div()
            .h(px(60.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .child(
                Self::pill(&theme, "Ramus").child(
                    div()
                        .px(px(8.))
                        .rounded(px(10.))
                        .bg(theme.tag_background)
                        .text_color(theme.tag_text)
                        .text_size(px(11.))
                        .child("GitLab"),
                ),
            )
            .child(Self::pill(&theme, "feat/diff-view"))
            .child(
                div()
                    .h(px(40.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(20.))
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .child("Push origin"),
            )
    }
}
