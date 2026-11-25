use gpui::{
    AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{repo_state::RepoState, theme::Theme};

/// Asks for the address of the remote to give a local-only repository.
pub struct AddRemoteDialog {
    repo_state: Entity<RepoState>,
    url: Entity<InputState>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for AddRemoteDialog {}

impl Focusable for AddRemoteDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AddRemoteDialog {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://github.com/owner/repo.git"));
        url.update(cx, |input, cx| input.focus(window, cx));
        let _subscription =
            cx.subscribe_in(
                &url,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.add(cx),
                    _ => {}
                },
            );
        Self {
            repo_state,
            url,
            focus_handle: cx.focus_handle(),
            _subscription,
        }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().trim().to_owned();
        if url.is_empty() {
            return;
        }
        self.repo_state
            .update(cx, |state, cx| state.add_origin(url, cx));
        cx.emit(DismissEvent);
    }
}

impl Render for AddRemoteDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let ready = !self.url.read(cx).value().trim().is_empty();
        let button = |id: &'static str| {
            div()
                .id(id)
                .h(px(theme.metrics.control_height))
                .px(px(20.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .cursor_pointer()
        };
        div()
            .id("add-remote")
            .track_focus(&self.focus_handle)
            .w(px(520.))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(theme.metrics.radius_md))
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                }
            }))
            .child(
                div()
                    .h(px(44.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(theme.metrics.text_lg))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Add a remote"),
            )
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(theme.metrics.text_sm))
                            .text_color(theme.text_muted)
                            .child("Address of the repository to push to (it becomes \"origin\")"),
                    )
                    .child(Input::new(&self.url).aria_label("Remote address")),
            )
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        button("cancel-add-remote")
                            .border_1()
                            .border_color(theme.border)
                            .hover(|b| b.bg(theme.surface_selected))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
                            .child("Cancel"),
                    )
                    .child(
                        button("confirm-add-remote")
                            .bg(theme.accent)
                            .text_color(theme.accent_text)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .when(!ready, |b| b.opacity(0.5))
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx)))
                            .child("Add remote"),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use gpui::{AppContext, TestAppContext};
    use tempfile::TempDir;

    use super::*;

    #[gpui::test]
    fn typing_an_address_and_pressing_enter_adds_origin_and_closes(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .arg(dir.path())
                .status()
                .unwrap()
                .success()
        );
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        cx.run_until_parked();
        assert!(!state.read_with(cx, |s, _| s.has_remote()));

        let (dialog, cx) = cx.add_window_view({
            let state = state.clone();
            move |window, cx| AddRemoteDialog::new(state, window, cx)
        });
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = closed.clone();
        cx.update(|_, cx| {
            cx.subscribe(&dialog, move |_, _: &DismissEvent, _| flag.set(true))
                .detach();
        });

        cx.simulate_keystrokes("enter");
        assert!(!closed.get(), "an empty address must not be accepted");

        cx.simulate_input("https://example.com/acme/thing.git");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(closed.get());
        assert!(state.read_with(cx, |s, _| s.has_remote()));
    }
}
