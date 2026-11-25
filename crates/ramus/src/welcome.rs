use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement,
    Styled, Window, div, px,
};

use crate::{
    actions::{CloneRepository, OpenRepository},
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
                                    .text_size(px(theme.metrics.text_title))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Open a repository to get started"),
                            )
                            .child(div().text_color(theme.text_muted).child(
                                "Ramus works on any Git repository. Hosting accounts are optional.",
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(action_row(
                                "open-repository",
                                "Open a local repository…",
                                "⌘ O",
                                &theme,
                                cx.listener(|_, _, window, cx| {
                                    window.dispatch_action(Box::new(OpenRepository), cx)
                                }),
                            ))
                            .child(action_row(
                                "clone-repository",
                                "Clone a repository…",
                                "⇧ ⌘ O",
                                &theme,
                                cx.listener(|_, _, window, cx| {
                                    window.dispatch_action(Box::new(CloneRepository), cx)
                                }),
                            )),
                    ),
            )
    }
}

/// One of the big buttons under the heading: a label and its keyboard shortcut.
fn action_row(
    id: &'static str,
    label: &'static str,
    shortcut: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(44.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(theme.metrics.radius_md))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .cursor_pointer()
        .hover(|style| style.bg(theme.surface_selected))
        .on_click(on_click)
        .child(
            div()
                .flex_1()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label),
        )
        .child(
            div()
                .px(px(6.))
                .rounded(px(theme.metrics.radius_sm))
                .border_1()
                .border_color(theme.border)
                .font_family(MONO_FONT)
                .text_size(px(theme.metrics.text_xs))
                .text_color(theme.text_muted)
                .child(shortcut),
        )
}
