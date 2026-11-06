use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::{
    actions::{OpenRepository, ToggleBranches},
    branch_popover::BranchPopover,
    changes_view::ChangesView,
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    title_bar::TitleBar,
    welcome::Welcome,
};

pub struct Workspace {
    repo_state: Entity<RepoState>,
    title_bar: Entity<TitleBar>,
    changes: Entity<ChangesView>,
    welcome: Entity<Welcome>,
    branch_popover: Option<Entity<BranchPopover>>,
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

        Self {
            title_bar: cx.new(|cx| TitleBar::new(repo_state.clone(), cx)),
            changes: cx.new(|cx| ChangesView::new(repo_state.clone(), window, cx)),
            welcome: cx.new(|_| Welcome),
            branch_popover: None,
            repo_state,
            focus_handle,
        }
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
            self.changes.clone().into_any_element()
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
            .child(body)
            .child(self.status_bar(&theme, cx))
            .children(popover)
    }
}
