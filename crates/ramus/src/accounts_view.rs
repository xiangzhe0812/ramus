use gpui::{
    ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    accounts::{AccountsState, Flow},
    theme::{MONO_FONT, Theme},
};

/// The Accounts dialog: who is signed in to GitHub, and the sign-in in progress.
pub struct AccountsView {
    accounts: Entity<AccountsState>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for AccountsView {}

impl Focusable for AccountsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AccountsView {
    pub fn new(
        accounts: Entity<AccountsState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            _subscription: cx.observe(&accounts, |_, _, cx| cx.notify()),
            accounts,
            focus_handle,
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        // Closing the dialog also abandons a sign-in nobody is looking at.
        self.accounts.update(cx, |accounts, cx| accounts.cancel(cx));
        cx.emit(DismissEvent);
    }
}

impl Render for AccountsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let state = self.accounts.read(cx);
        let account = state.account().cloned();
        let flow = state.flow().clone();
        let page = state.verification_page();

        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(theme.metrics.control_height))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .cursor_pointer()
                .border_1()
                .border_color(theme.border)
                .hover(|b| b.bg(theme.surface_selected))
                .child(label)
        };
        let accounts = self.accounts.clone();

        let detail = match (&account, &flow) {
            (Some(account), _) => format!("github.com · {}", account.login),
            (None, Flow::Requesting) => "Contacting GitHub…".to_owned(),
            (None, Flow::Waiting { .. }) => {
                "Waiting for you to approve it on github.com".to_owned()
            }
            (None, _) => "Not signed in".to_owned(),
        };

        let action = match (&account, &flow) {
            (_, Flow::Requesting | Flow::Waiting { .. }) => button("cancel-sign-in", "Cancel")
                .on_click({
                    let accounts = accounts.clone();
                    move |_, _, cx| accounts.update(cx, |a, cx| a.cancel(cx))
                })
                .into_any_element(),
            (Some(_), _) => button("sign-out", "Sign out")
                .on_click({
                    let accounts = accounts.clone();
                    move |_, _, cx| accounts.update(cx, |a, cx| a.sign_out(cx))
                })
                .into_any_element(),
            (None, _) => button("sign-in", "Sign in")
                .bg(theme.accent)
                .text_color(theme.accent_text)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .on_click({
                    let accounts = accounts.clone();
                    move |_, _, cx| accounts.update(cx, |a, cx| a.sign_in(cx))
                })
                .into_any_element(),
        };

        div()
            .id("accounts")
            .track_focus(&self.focus_handle)
            .w(px(560.))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(theme.metrics.radius_md))
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close(cx);
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
                    .child("Accounts"),
            )
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .min_h(px(48.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .rounded(px(theme.metrics.radius_md))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                div()
                                    .size(px(32.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(theme.metrics.radius_md))
                                    .bg(theme.border)
                                    .text_size(px(theme.metrics.text_xs))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("GH"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div().font_weight(gpui::FontWeight::MEDIUM).child("GitHub"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme.metrics.text_sm))
                                            .text_color(theme.text_muted)
                                            .child(SharedString::from(detail)),
                                    ),
                            )
                            .child(action),
                    )
                    .children(match &flow {
                        Flow::Waiting { user_code } => {
                            let (code, page) = (user_code.clone(), page.clone());
                            Some(
                                div()
                                    .p(px(14.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .rounded(px(theme.metrics.radius_md))
                                    .border_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .text_color(theme.text_muted)
                                            .child("Open GitHub and enter this code:"),
                                    )
                                    .child(
                                        div()
                                            .font_family(MONO_FONT)
                                            .text_size(px(theme.metrics.text_title))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(SharedString::from(code.clone())),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(8.))
                                            .child(
                                                button("open-github", "Open GitHub")
                                                    .on_click(move |_, _, cx| cx.open_url(&page)),
                                            )
                                            .child(button("copy-code", "Copy code").on_click(
                                                move |_, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(code.clone()),
                                                    )
                                                },
                                            )),
                                    )
                                    .into_any_element(),
                            )
                        }
                        Flow::Failed(message) => Some(
                            div()
                                .text_size(px(theme.metrics.text_sm))
                                .text_color(theme.error_text)
                                .child(SharedString::from(message.clone()))
                                .into_any_element(),
                        ),
                        _ => None,
                    })
                    .when(account.is_some(), |body| {
                        body.child(
                            div()
                                .text_size(px(theme.metrics.text_sm))
                                .text_color(theme.text_muted)
                                .child(
                                    "Signing out forgets the token on this Mac. To revoke it on \
                                     GitHub, remove Ramus at github.com/settings/applications.",
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .justify_end()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        button("close-accounts", "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    use gpui::{AppContext, TestAppContext};

    use super::*;
    use crate::{github::Endpoints, secrets::MemoryStore};

    #[gpui::test]
    fn escape_closes_and_abandons_a_sign_in(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let accounts = cx.new(|_| {
            AccountsState::new(
                Arc::new(MemoryStore::default()),
                Endpoints::default(),
                String::new(),
            )
        });
        let (view, cx) = cx.add_window_view({
            let accounts = accounts.clone();
            move |window, cx| AccountsView::new(accounts, window, cx)
        });
        let closed = Rc::new(Cell::new(false));
        let flag = closed.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, _: &DismissEvent, _| flag.set(true))
                .detach();
        });
        // An empty client ID fails at once, which renders the failure message.
        accounts.update(cx, |a, cx| a.sign_in(cx));
        cx.run_until_parked();
        assert!(matches!(
            accounts.read_with(cx, |a, _| a.flow().clone()),
            Flow::Failed(_)
        ));

        cx.simulate_keystrokes("escape");
        assert!(closed.get());
    }
}
