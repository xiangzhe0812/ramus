use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use git_core::FailureKind;

use crate::{
    accounts::AccountsState,
    accounts_view::AccountsView,
    actions::{
        AddRemote, CloneRepository, DiscardSelected, NewBranchHere, OpenRepository, ShowAccounts,
        ShowChanges, ShowHistory, ShowStashes, ToggleBranches, TogglePalette,
    },
    add_remote_dialog::AddRemoteDialog,
    branch_popover::BranchPopover,
    changes_view::{ChangesEvent, ChangesView},
    clone_dialog::CloneDialog,
    conflicts_view::{ConflictsView, abort_label, operation_summary},
    error_banner,
    history_view::HistoryView,
    palette::{Command, Target},
    palette_view::{PaletteView, RunEntry},
    repo_state::RepoState,
    stash_view::StashView,
    theme::{MONO_FONT, Theme},
    title_bar::{Tab, TabRequested, TitleBar},
    welcome::Welcome,
};

/// How often the open repository is fetched in the background.
const AUTO_FETCH_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

/// An overlay drawn over the window. Only one can be open at a time, so opening any of
/// them closes whichever was showing.
enum Modal {
    Branches(Entity<BranchPopover>),
    Palette(Entity<PaletteView>),
    Clone(Entity<CloneDialog>),
    Accounts(Entity<AccountsView>),
    AddRemote(Entity<AddRemoteDialog>),
}

pub struct Workspace {
    repo_state: Entity<RepoState>,
    title_bar: Entity<TitleBar>,
    changes: Entity<ChangesView>,
    history: Entity<HistoryView>,
    conflicts: Entity<ConflictsView>,
    stashes: Entity<StashView>,
    tab: Tab,
    welcome: Entity<Welcome>,
    /// The one overlay that can be open (branches, palette, Clone, Accounts, Add remote).
    modal: Option<Modal>,
    accounts: Entity<AccountsState>,
    error_details_open: bool,
    /// Files the user asked to discard, waiting for them to confirm.
    pending_discard: Option<Vec<git_core::FileChange>>,
    focus_handle: FocusHandle,
}

impl Workspace {
    pub fn new(
        repo_state: Entity<RepoState>,
        accounts: Entity<AccountsState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&repo_state, |_, _, cx| cx.notify()).detach();

        // Edits made outside the app (a terminal commit, an editor save) show up on return.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.repo_state.update(cx, |state, cx| state.refresh(cx));
            }
        })
        .detach();

        // Look for news on the remote now and then, so "behind" stays honest.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTO_FETCH_EVERY).await;
                let alive = this.update(cx, |this, cx| {
                    this.repo_state.update(cx, |state, cx| state.auto_fetch(cx));
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let changes = cx.new(|cx| ChangesView::new(repo_state.clone(), window, cx));
        let history = cx.new(|cx| HistoryView::new(repo_state.clone(), window, cx));
        let stashes = cx.new(|cx| StashView::new(repo_state.clone(), window, cx));
        let conflicts = cx.new(|cx| ConflictsView::new(repo_state.clone(), window, cx));
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
            conflicts,
            stashes,
            tab: Tab::Changes,
            welcome: cx.new(|_| Welcome),
            modal: None,
            accounts,
            error_details_open: false,
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
                .bg(theme.scrim_strong)
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

    /// Closes whatever overlay is open and gives the keyboard back to the workspace
    /// (its shortcuts only work while it holds focus).
    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.modal.take() {
            // A sign-in nobody can see any more shouldn't keep polling.
            if matches!(modal, Modal::Accounts(_)) {
                self.accounts.update(cx, |accounts, cx| accounts.cancel(cx));
            }
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Shows `modal` as the only overlay. The caller closes the previous one *before*
    /// building the new view, because closing hands focus back to the workspace and would
    /// take it from the new view's input.
    fn show_modal(&mut self, modal: Modal, cx: &mut Context<Self>) {
        self.modal = Some(modal);
        cx.notify();
    }

    /// Closes the overlay when the view it shows asks to be dismissed.
    fn close_on_dismiss<V: gpui::EventEmitter<gpui::DismissEvent> + 'static>(
        &self,
        view: &Entity<V>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            view,
            window,
            |this, _, _: &gpui::DismissEvent, window, cx| this.close_modal(window, cx),
        )
        .detach();
    }

    fn open_add_remote(&mut self, _: &AddRemote, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::AddRemote(_)))
            || self.repo_state.read(cx).repo().is_none()
        {
            return;
        }
        self.close_modal(window, cx);
        let dialog = cx.new(|cx| AddRemoteDialog::new(self.repo_state.clone(), window, cx));
        self.close_on_dismiss(&dialog, window, cx);
        self.show_modal(Modal::AddRemote(dialog), cx);
    }

    /// Opens the branch popover already asking for a new branch's name.
    fn new_branch_here(&mut self, _: &NewBranchHere, window: &mut Window, cx: &mut Context<Self>) {
        self.close_modal(window, cx);
        self.toggle_branches(&ToggleBranches, window, cx);
        if let Some(Modal::Branches(popover)) = &self.modal {
            popover.update(cx, |popover, cx| popover.start_creating(window, cx));
        }
    }

    fn open_accounts(&mut self, _: &ShowAccounts, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Accounts(_))) {
            return;
        }
        self.close_modal(window, cx);
        let view = cx.new(|cx| AccountsView::new(self.accounts.clone(), window, cx));
        self.close_on_dismiss(&view, window, cx);
        self.show_modal(Modal::Accounts(view), cx);
    }

    fn open_clone(&mut self, _: &CloneRepository, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Clone(_))) {
            return;
        }
        self.close_modal(window, cx);
        let dialog = cx.new(|cx| CloneDialog::new(self.repo_state.clone(), window, cx));
        self.close_on_dismiss(&dialog, window, cx);
        self.show_modal(Modal::Clone(dialog), cx);
    }

    fn toggle_palette(&mut self, _: &TogglePalette, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Palette(_))) {
            self.close_modal(window, cx);
            return;
        }
        self.close_modal(window, cx);
        let palette = cx.new(|cx| PaletteView::new(self.repo_state.clone(), window, cx));
        self.close_on_dismiss(&palette, window, cx);
        cx.subscribe_in(&palette, window, |this, _, event: &RunEntry, window, cx| {
            // Close first: the commands below expect the workspace to hold focus.
            this.close_modal(window, cx);
            this.run(event.0.clone(), window, cx);
        })
        .detach();
        self.show_modal(Modal::Palette(palette), cx);
    }

    fn run(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let command = match target {
            Target::SwitchTo(branch) => {
                self.repo_state
                    .update(cx, |state, cx| state.switch_branch(branch, cx));
                return;
            }
            Target::Command(command) => command,
        };
        match command {
            Command::OpenRepository => window.dispatch_action(Box::new(OpenRepository), cx),
            Command::CloneRepository => window.dispatch_action(Box::new(CloneRepository), cx),
            Command::ShowAccounts => window.dispatch_action(Box::new(ShowAccounts), cx),
            Command::SwitchBranch => window.dispatch_action(Box::new(ToggleBranches), cx),
            Command::ShowChanges => self.show(Tab::Changes, cx),
            Command::ShowHistory => self.show(Tab::History, cx),
            Command::ShowStashes => self.show(Tab::Stashes, cx),
            Command::StashChanges => self
                .repo_state
                .update(cx, |state, cx| state.stash_changes(String::new(), cx)),
            Command::DiscardSelected => window.dispatch_action(Box::new(DiscardSelected), cx),
            Command::Sync(kind) => self.repo_state.update(cx, |state, cx| state.sync(kind, cx)),
            Command::StageAll => self.repo_state.update(cx, |state, cx| state.stage_all(cx)),
            Command::UnstageAll => self
                .repo_state
                .update(cx, |state, cx| state.unstage_all(cx)),
        }
    }

    fn toggle_branches(&mut self, _: &ToggleBranches, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Branches(_))) {
            self.close_modal(window, cx);
            return;
        }
        if self.repo_state.read(cx).repo().is_none() {
            return;
        }
        self.close_modal(window, cx);
        let popover = cx.new(|cx| BranchPopover::new(self.repo_state.clone(), window, cx));
        self.close_on_dismiss(&popover, window, cx);
        self.show_modal(Modal::Branches(popover), cx);
    }

    /// The layer drawn over everything for the open overlay.
    fn modal_layer(&self, modal: &Modal, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        // Centered dialogs sit on a dimmed scrim. `dismiss_on_outside_click` is off where
        // losing a half-typed address to a stray click would be worse than a closed dialog.
        let scrim =
            |id: &'static str, top: f32, dismiss_on_outside_click: bool, child: AnyElement| {
                div()
                    .id(id)
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .pt(px(top))
                    .bg(theme.scrim)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if dismiss_on_outside_click {
                                this.close_modal(window, cx);
                            } else {
                                cx.stop_propagation();
                            }
                        }),
                    )
                    .child(child)
                    .into_any_element()
            };
        match modal {
            // The branch popover hangs under the branch button on a clear layer: a click
            // anywhere else closes it.
            Modal::Branches(popover) => div()
                .id("popover-backdrop")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_modal(window, cx)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(64.))
                        .left(px(246.))
                        .child(popover.clone()),
                )
                .into_any_element(),
            Modal::Palette(palette) => scrim(
                "palette-backdrop",
                80.,
                true,
                palette.clone().into_any_element(),
            ),
            Modal::Accounts(view) => scrim(
                "accounts-backdrop",
                120.,
                true,
                view.clone().into_any_element(),
            ),
            Modal::Clone(dialog) => scrim(
                "clone-backdrop",
                120.,
                false,
                dialog.clone().into_any_element(),
            ),
            Modal::AddRemote(dialog) => scrim(
                "add-remote-backdrop",
                120.,
                false,
                dialog.clone().into_any_element(),
            ),
        }
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
        let state = self.repo_state.read(cx);
        let message = state.error()?.clone();
        let present = error_banner::present(&message);
        let idle = state.operation().is_none();
        let offer_fix = present.kind == FailureKind::Rejected && state.is_trusted() && idle;
        let offer_retry = present.kind != FailureKind::Rejected && state.can_retry();
        let open = self.error_details_open && present.has_details;

        let outline = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(32.))
                .px(px(14.))
                .flex()
                .items_center()
                .rounded(px(16.))
                .border_1()
                .border_color(theme.error_border)
                .cursor_pointer()
                .hover(|b| b.bg(theme.error_border))
                .child(label)
        };

        Some(
            div()
                .mx(px(16.))
                .mb(px(12.))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(theme.error_border)
                .bg(theme.error_background)
                .text_color(theme.error_text)
                .child(
                    div()
                        .min_h(px(48.))
                        .px(px(16.))
                        .py(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(SharedString::from(present.title.clone())),
                                )
                                .children(present.hint.clone().map(|hint| {
                                    div().text_size(px(12.)).child(SharedString::from(hint))
                                })),
                        )
                        .when(offer_fix, |row| {
                            row.child(
                                outline("pull-then-push", "Pull, then push")
                                    .bg(theme.accent)
                                    .border_color(theme.accent)
                                    .text_color(theme.accent_text)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.repo_state
                                            .update(cx, |state, cx| state.pull_then_push(cx));
                                    })),
                            )
                        })
                        .when(offer_retry, |row| {
                            row.child(outline("retry-sync", "Retry").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.repo_state.update(cx, |state, cx| state.retry_sync(cx));
                                },
                            )))
                        })
                        .when(present.has_details, |row| {
                            row.child(
                                outline(
                                    "error-details",
                                    if open { "Details ▾" } else { "Details ▸" },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.error_details_open = !this.error_details_open;
                                        cx.notify();
                                    },
                                )),
                            )
                        })
                        .child(outline("dismiss-error", "Dismiss").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.error_details_open = false;
                                this.repo_state
                                    .update(cx, |state, cx| state.dismiss_error(cx));
                            },
                        ))),
                )
                .when(open, |banner| {
                    banner.child(
                        div()
                            .px(px(16.))
                            .py(px(10.))
                            .border_t_1()
                            .border_color(theme.error_border)
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .child(message),
                    )
                }),
        )
    }

    /// Shown on every tab while a merge, cherry-pick or revert waits on its conflicts.
    fn operation_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.repo_state.read(cx);
        let progress = state.in_progress()?.clone();
        let branch = state
            .status()
            .map(|status| match &status.branch.head {
                git_core::Head::Branch(name) => name.clone(),
                git_core::Head::Detached(sha) => format!("detached {sha}"),
            })
            .unwrap_or_default();
        let open = state.conflicted_paths().len();
        let text = operation_summary(progress.kind, progress.merging.as_deref(), &branch, open);
        Some(
            div()
                .mx(px(16.))
                .mb(px(12.))
                .h(px(48.))
                .px(px(16.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(theme.modified.text)
                .bg(theme.modified.background)
                .text_color(theme.modified.text)
                .child(div().flex_1().min_w_0().truncate().child(text))
                .child(
                    div()
                        .id("abort-operation")
                        .h(px(32.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(theme.modified.text)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.repo_state
                                .update(cx, |state, cx| state.abort_operation(cx));
                        }))
                        .child(abort_label(progress.kind)),
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

        let resolving = {
            let state = self.repo_state.read(cx);
            state.in_progress().is_some() || !state.conflicted_paths().is_empty()
        };
        let body = if has_repo {
            match self.tab {
                // While a merge waits on conflicts, the Changes tab is where they are resolved.
                Tab::Changes if resolving => self.conflicts.clone().into_any_element(),
                Tab::Changes => self.changes.clone().into_any_element(),
                Tab::History => self.history.clone().into_any_element(),
                Tab::Stashes => self.stashes.clone().into_any_element(),
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

        let modal = self
            .modal
            .as_ref()
            .map(|modal| self.modal_layer(modal, &theme, cx));

        div()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_repository))
            .on_action(cx.listener(Self::toggle_branches))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::open_clone))
            .on_action(cx.listener(Self::open_accounts))
            .on_action(cx.listener(Self::open_add_remote))
            .on_action(cx.listener(Self::new_branch_here))
            .on_action(cx.listener(Self::discard_selected))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.show(Tab::Changes, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, _, cx| this.show(Tab::History, cx)))
            .on_action(cx.listener(|this, _: &ShowStashes, _, cx| this.show(Tab::Stashes, cx)))
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
            .children(self.operation_banner(&theme, cx))
            .children(self.error_banner(&theme, cx))
            .children(self.undo_banner(&theme, cx))
            .child(body)
            .child(self.status_bar(&theme, cx))
            .children(modal)
            .children(self.discard_dialog(&theme, cx))
    }
}

#[cfg(test)]
impl Workspace {
    /// Which overlay is showing, by name.
    fn modal_name(&self) -> &'static str {
        match self.modal {
            None => "none",
            Some(Modal::Branches(_)) => "branches",
            Some(Modal::Palette(_)) => "palette",
            Some(Modal::Clone(_)) => "clone",
            Some(Modal::Accounts(_)) => "accounts",
            Some(Modal::AddRemote(_)) => "add-remote",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Action, TestAppContext, VisualTestContext};

    use super::*;
    use crate::{github::Endpoints, secrets::MemoryStore, testutil};

    /// A workspace over a real repository, with an in-memory account store, so nothing
    /// here can reach the user's keychain.
    fn open<'a>(
        cx: &'a mut TestAppContext,
        dir: &std::path::Path,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        testutil::git(dir, &["init", "-q", "-b", "main"]);
        testutil::git(dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let repo_state = cx.new(|_| RepoState::default());
        repo_state.update(cx, |state, cx| state.open(dir.to_owned(), cx));
        cx.run_until_parked();
        let accounts = cx.new(|_| {
            AccountsState::new(
                Arc::new(MemoryStore::default()),
                Endpoints::default(),
                crate::github::PLACEHOLDER_CLIENT_ID.into(),
            )
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(repo_state, accounts, window, cx));
        cx.run_until_parked();
        (workspace, cx)
    }

    fn show(cx: &mut VisualTestContext, action: impl Action) {
        cx.dispatch_action(action);
        cx.run_until_parked();
    }

    fn name(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> &'static str {
        workspace.read_with(cx, |w, _| w.modal_name())
    }

    #[gpui::test]
    fn each_overlay_replaces_whichever_was_open(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().unwrap();
        let (workspace, cx) = open(cx, dir.path());
        assert_eq!(name(&workspace, cx), "none");

        // Every overlay opened over every other one: only the newest may remain.
        type Opener = fn(&mut VisualTestContext);
        let openers: [(&str, Opener); 5] = [
            ("clone", |cx| show(cx, CloneRepository)),
            ("branches", |cx| show(cx, ToggleBranches)),
            ("accounts", |cx| show(cx, ShowAccounts)),
            ("palette", |cx| show(cx, TogglePalette)),
            ("add-remote", |cx| show(cx, AddRemote)),
        ];
        for (first, open_first) in &openers {
            for (second, open_second) in &openers {
                if first == second {
                    continue;
                }
                open_first(cx);
                assert_eq!(name(&workspace, cx), *first, "opening {first}");
                open_second(cx);
                assert_eq!(
                    name(&workspace, cx),
                    *second,
                    "{second} opened over {first} should replace it"
                );
                // Back to nothing for the next pair.
                cx.simulate_keystrokes("escape");
                cx.run_until_parked();
                assert_eq!(name(&workspace, cx), "none", "escape closes {second}");
            }
        }
    }

    #[gpui::test]
    fn the_shortcut_for_an_open_overlay_closes_it(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().unwrap();
        let (workspace, cx) = open(cx, dir.path());
        show(cx, TogglePalette);
        assert_eq!(name(&workspace, cx), "palette");
        show(cx, TogglePalette);
        assert_eq!(name(&workspace, cx), "none");
        show(cx, ToggleBranches);
        show(cx, ToggleBranches);
        assert_eq!(name(&workspace, cx), "none");
    }

    #[gpui::test]
    fn new_branch_here_replaces_the_palette_with_the_branch_popover(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().unwrap();
        let (workspace, cx) = open(cx, dir.path());
        show(cx, TogglePalette);
        show(cx, NewBranchHere);
        assert_eq!(name(&workspace, cx), "branches");
    }
}
