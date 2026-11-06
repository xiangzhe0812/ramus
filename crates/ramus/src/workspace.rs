use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::{
    actions::{DiscardSelected, OpenRepository, ShowChanges, ShowHistory, ToggleBranches},
    branch_popover::BranchPopover,
    changes_view::{ChangesEvent, ChangesView},
    history_view::HistoryView,
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    title_bar::{Tab, TabRequested, TitleBar},
    welcome::Welcome,
};

pub struct Workspace {
    repo_state: Entity<RepoState>,
    title_bar: Entity<TitleBar>,
    changes: Entity<ChangesView>,
    history: Entity<HistoryView>,
    tab: Tab,
    welcome: Entity<Welcome>,
    branch_popover: Option<Entity<BranchPopover>>,
    /// Files the user asked to discard, waiting for them to confirm.
    pending_discard: Option<Vec<git_core::FileChange>>,
    focus_handle: FocusHandle,
}

impl Workspace {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&repo_state, |_, _, cx| cx.notify()).detach();

        // Edits made outside the app (a terminal commit, an editor save) show up on return.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.repo_state.update(cx, |state, cx| state.refresh(cx));
            }
        })
        .detach();

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let changes = cx.new(|cx| ChangesView::new(repo_state.clone(), window, cx));
        let history = cx.new(|cx| HistoryView::new(repo_state.clone(), window, cx));
        let title_bar = cx.new(|cx| TitleBar::new(repo_state.clone(), cx));
        cx.subscribe(&title_bar, |this, _, event: &TabRequested, cx| {
            this.show(event.0, cx)
        })
        .detach();
        cx.subscribe_in(
            &changes,
            window,
            |this, _, event: &ChangesEvent, window, cx| {
                let ChangesEvent::DiscardRequested(files) = event;
                this.pending_discard = Some(files.clone());
                // Keys (Enter, Escape) are handled by the workspace, so it must hold focus.
                window.focus(&this.focus_handle, cx);
                cx.notify();
            },
        )
        .detach();

        Self {
            title_bar,
            changes,
            history,
            tab: Tab::Changes,
            welcome: cx.new(|_| Welcome),
            branch_popover: None,
            pending_discard: None,
            repo_state,
            focus_handle,
        }
    }

    fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.title_bar.update(cx, |bar, cx| bar.set_tab(tab, cx));
        cx.notify();
    }

    fn confirm_discard(&mut self, cx: &mut Context<Self>) {
        if let Some(files) = self.pending_discard.take() {
            self.repo_state
                .update(cx, |state, cx| state.discard(files, cx));
            cx.notify();
        }
    }

    fn cancel_discard(&mut self, cx: &mut Context<Self>) {
        if self.pending_discard.take().is_some() {
            cx.notify();
        }
    }

    fn discard_selected(&mut self, _: &DiscardSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.changes
            .update(cx, |changes, cx| changes.request_discard(cx));
    }

    /// The confirmation shown before anything is discarded.
    fn discard_dialog(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let files = self.pending_discard.as_ref()?;
        let what = match files.as_slice() {
            [one] => one.path.clone(),
            many => format!("{} files", many.len()),
        };
        let button = |id: &'static str| {
            div()
                .id(id)
                .h(px(36.))
                .px(px(16.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .cursor_pointer()
                .font_weight(gpui::FontWeight::SEMIBOLD)
        };
        Some(
            div()
                .id("discard-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000099))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.cancel_discard(cx)),
                )
                .child(
                    div()
                        .w(px(440.))
                        .p(px(20.))
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Discard changes?"),
                        )
                        .child(div().text_color(theme.text_muted).child(format!(
                            "Your changes to {what} will be removed. Ramus keeps a backup, so you can undo this right afterwards."
                        )))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    button("cancel-discard")
                                        .border_1()
                                        .border_color(theme.border)
                                        .on_click(cx.listener(|this, _, _, cx| this.cancel_discard(cx)))
                                        .child("Cancel"),
                                )
                                .child(
                                    button("confirm-discard")
                                        .bg(theme.deleted.background)
                                        .text_color(theme.deleted.text)
                                        .on_click(cx.listener(|this, _, _, cx| this.confirm_discard(cx)))
                                        .child("Discard"),
                                ),
                        ),
                ),
        )
    }

    /// "Discarded changes to x. Undo", shown until dismissed or replaced.
    fn undo_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let message = self.repo_state.read(cx).last_discard()?.to_owned();
        let action = |id: &'static str| {
            div()
                .id(id)
                .h(px(32.))
                .px(px(14.))
                .flex()
                .items_center()
                .rounded(px(16.))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
        };
        Some(
            div()
                .mx(px(16.))
                .mb(px(12.))
                .min_h(px(48.))
                .px(px(16.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .child(div().flex_1().child(message))
                .child(
                    action("undo-discard")
                        .bg(theme.accent)
                        .text_color(theme.accent_text)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.repo_state
                                .update(cx, |state, cx| state.undo_discard(cx));
                        }))
                        .child("Undo"),
                )
                .child(
                    action("dismiss-discard")
                        .text_color(theme.text_muted)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.repo_state
                                .update(cx, |state, cx| state.dismiss_discard(cx));
                        }))
                        .child("Dismiss"),
                ),
        )
    }

    fn close_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_popover.take().is_some() {
            // Keyboard shortcuts work from the workspace, so hand focus back to it.
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn toggle_branches(&mut self, _: &ToggleBranches, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_popover.is_some() {
            self.close_branches(window, cx);
            return;
        }
        if self.repo_state.read(cx).repo().is_none() {
            return;
        }
        let popover = cx.new(|cx| BranchPopover::new(self.repo_state.clone(), window, cx));
        cx.subscribe_in(
            &popover,
            window,
            |this, _, _: &gpui::DismissEvent, window, cx| this.close_branches(window, cx),
        )
        .detach();
        self.branch_popover = Some(popover);
        cx.notify();
    }

    fn open_repository(&mut self, _: &OpenRepository, _: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        let repo_state = self.repo_state.clone();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                repo_state.update(cx, |state, cx| state.open(path, cx));
            }
        })
        .detach();
    }

    fn error_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let message = self.repo_state.read(cx).error()?.clone();
        Some(
            div()
                .mx(px(16.))
                .mb(px(12.))
                .min_h(px(48.))
                .px(px(16.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(theme.error_border)
                .bg(theme.error_background)
                .text_color(theme.error_text)
                .child(div().flex_1().child(message))
                .child(
                    div()
                        .id("dismiss-error")
                        .h(px(32.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(theme.error_border)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.repo_state
                                .update(cx, |state, cx| state.dismiss_error(cx));
                        }))
                        .child("Dismiss"),
                ),
        )
    }

    /// Shown while the open repository isn't trusted yet.
    fn restricted_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.repo_state.read(cx);
        if state.repo().is_none() || state.is_trusted() {
            return None;
        }
        Some(
            div()
                .mx(px(16.))
                .mb(px(12.))
                .min_h(px(48.))
                .px(px(16.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(theme.modified.text)
                .bg(theme.modified.background)
                .text_color(theme.modified.text)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Restricted mode"),
                        )
                        .child(div().text_size(px(12.)).child(
                            "You can view changes and stage files. Committing and syncing are off, \
                             because this repository's own settings can run programs (hooks, credential \
                             helpers). Trust it only if you trust where it came from.",
                        )),
                )
                .child(
                    div()
                        .id("trust-repository")
                        .h(px(32.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .rounded(px(16.))
                        .bg(theme.accent)
                        .text_color(theme.accent_text)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.repo_state.update(cx, |state, cx| state.trust(cx));
                        }))
                        .child("Trust repository"),
                ),
        )
    }

    fn status_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.repo_state.read(cx);
        let summary = state.status().map(|status| {
            let branch = &status.branch;
            let sync = match &branch.upstream {
                Some(upstream) => format!(
                    "{} ahead · {} behind {upstream}",
                    branch.ahead, branch.behind
                ),
                None => "No upstream branch".to_owned(),
            };
            let changes = match status.files.len() {
                0 => "Working tree clean".to_owned(),
                1 => "1 uncommitted change".to_owned(),
                n => format!("{n} uncommitted changes"),
            };
            (sync, changes)
        });
        let path = state.repo().map(|repo| {
            let path = repo.root().display().to_string();
            match std::env::var("HOME") {
                Ok(home) if path.starts_with(&home) => path.replacen(&home, "~", 1),
                _ => path,
            }
        });

        div()
            .h(px(28.))
            .flex_none()
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(16.))
            .text_size(px(12.))
            .text_color(theme.text_muted)
            .when_some(summary, |bar, (sync, changes)| {
                bar.child(sync).child(changes)
            })
            .when(path.is_none(), |bar| bar.child("No repository open"))
            .child(div().flex_1())
            .when_some(path, |bar, path| {
                bar.child(div().font_family(MONO_FONT).child(path))
            })
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let has_repo = self.repo_state.read(cx).repo().is_some();

        let body = if has_repo {
            match self.tab {
                Tab::Changes => self.changes.clone().into_any_element(),
                Tab::History => self.history.clone().into_any_element(),
            }
        } else {
            div()
                .flex_1()
                .min_h_0()
                .px(px(16.))
                .pb(px(12.))
                .child(
                    div()
                        .size_full()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme.border)
                        .overflow_hidden()
                        .child(self.welcome.clone()),
                )
                .into_any_element()
        };

        let popover = self.branch_popover.clone().map(|popover| {
            // A transparent layer under the popover: a click anywhere else closes it.
            div()
                .id("popover-backdrop")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_branches(window, cx)),
                )
                .child(div().absolute().top(px(64.)).left(px(246.)).child(popover))
        });

        div()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_repository))
            .on_action(cx.listener(Self::toggle_branches))
            .on_action(cx.listener(Self::discard_selected))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.show(Tab::Changes, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, _, cx| this.show(Tab::History, cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.pending_discard.is_some() {
                    match event.keystroke.key.as_str() {
                        "escape" => this.cancel_discard(cx),
                        "enter" => this.confirm_discard(cx),
                        _ => {}
                    }
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_size(px(13.))
            .child(self.title_bar.clone())
            .children(self.restricted_banner(&theme, cx))
            .children(self.error_banner(&theme, cx))
            .children(self.undo_banner(&theme, cx))
            .child(body)
            .child(self.status_bar(&theme, cx))
            .children(popover)
            .children(self.discard_dialog(&theme, cx))
    }
}
