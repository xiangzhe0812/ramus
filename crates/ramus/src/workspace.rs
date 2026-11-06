use gpui::{AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};

use crate::{theme::Theme, title_bar::TitleBar};

pub struct Workspace {
    title_bar: Entity<TitleBar>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            title_bar: cx.new(|_| TitleBar),
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_size(gpui::px(13.))
            .child(self.title_bar.clone())
    }
}
