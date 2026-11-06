use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement,
    Styled, Window, div, px,
};

use crate::{
    actions::OpenRepository,
    theme::{MONO_FONT, Theme},
};

/// Shown when no repository is open.
pub struct Welcome;

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.surface)
            .child(
                div()
                    .w(px(560.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Open a repository to get started"),
                            )
                            .child(div().text_color(theme.text_muted).child(
                                "Ramus works on any Git repository. Hosting accounts are optional.",
                            )),
                    )
                    .child(
                        div()
                            .id("open-repository")
                            .h(px(56.))
                            .px(px(16.))
                            .flex()
                            .items_center()
                            .rounded(px(10.))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.surface_selected))
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(OpenRepository), cx)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Open a local repository…"),
                            )
                            .child(
                                div()
                                    .px(px(6.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .font_family(MONO_FONT)
                                    .text_size(px(11.))
                                    .text_color(theme.text_muted)
                                    .child("⌘ O"),
                            ),
                    ),
            )
    }
}
