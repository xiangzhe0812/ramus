use std::path::{Path, PathBuf};

use gpui::{
    AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
};

/// The folder name git would pick for `url`: the last path part without `.git`.
/// `https://host/a/b.git`, `git@host:a/b.git`, `/path/to/b/` and `b` all give `b`.
pub fn repo_name_from_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let last = url.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    // A name that could escape the chosen folder, or is only dots, is no name.
    let usable = !name.is_empty() && name != "." && name != ".." && !name.contains('\\');
    usable.then(|| name.to_owned())
}

/// Where the clone will go, if there is enough to say.
fn destination(parent: &Path, url: &str) -> Option<PathBuf> {
    repo_name_from_url(url).map(|name| parent.join(name))
}

/// The "Clone a repository" dialog: an address, a folder, and progress.
pub struct CloneDialog {
    repo_state: Entity<RepoState>,
    url: Entity<InputState>,
    parent: PathBuf,
    /// A clone started from this dialog is running or has just ended.
    started: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for CloneDialog {}

impl Focusable for CloneDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CloneDialog {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://github.com/owner/repo.git"));
        url.update(cx, |input, cx| input.focus(window, cx));

        let _subscriptions = vec![
            cx.observe(&repo_state, |this, state, cx| {
                // Our clone ended: close on success, stay (showing why) on failure.
                if this.started && state.read(cx).operation().is_none() {
                    this.started = false;
                    if state.read(cx).error().is_none() {
                        cx.emit(DismissEvent);
                    }
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &url,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.start(cx),
                    _ => {}
                },
            ),
        ];

        Self {
            repo_state,
            url,
            parent: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            started: false,
            focus_handle: cx.focus_handle(),
            _subscriptions,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.repo_state.read(cx).operation().is_some() {
            return;
        }
        let url = self.url.read(cx).value().trim().to_owned();
        let Some(destination) = destination(&self.parent, &url) else {
            return;
        };
        self.started = true;
        // A failure from an earlier attempt would otherwise look like this one's.
        self.repo_state
            .update(cx, |state, cx| state.dismiss_error(cx));
        self.repo_state
            .update(cx, |state, cx| state.clone_repository(url, destination, cx));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let running = self.repo_state.read(cx).operation().is_some();
        if running {
            self.repo_state
                .update(cx, |state, cx| state.cancel_sync(cx));
        } else {
            cx.emit(DismissEvent);
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update(cx, |this, cx| {
                    this.parent = path;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for CloneDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let url = self.url.read(cx).value().to_string();
        let target = destination(&self.parent, &url);
        let state = self.repo_state.read(cx);
        let busy = state.operation().is_some();
        let progress = state.operation().map(|operation| {
            if operation.cancelling {
                "Cancelling…".to_owned()
            } else {
                match &operation.progress {
                    Some(progress) => format!("Cloning… {}%", progress.percent),
                    None => "Cloning…".to_owned(),
                }
            }
        });
        // Only this dialog's own failure belongs here.
        let error = (!busy).then(|| state.error().cloned()).flatten();

        let button = |id: &'static str| {
            div()
                .id(id)
                .h(px(40.))
                .px(px(20.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
        };

        div()
            .id("clone-dialog")
            .track_focus(&self.focus_handle)
            .w(px(560.))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(cx);
                }
            }))
            .child(
                div()
                    .h(px(52.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(15.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Clone a repository"),
            )
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child("Repository address"),
                    )
                    .child(Input::new(&self.url).aria_label("Repository address"))
                    .child(
                        div()
                            .mt(px(8.))
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child("Clone into"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(MONO_FONT)
                                    .text_size(px(12.5))
                                    .child(SharedString::from(match &target {
                                        Some(path) => path.display().to_string(),
                                        None => self.parent.display().to_string(),
                                    })),
                            )
                            .child(
                                button("choose-folder")
                                    .h(px(36.))
                                    .px(px(14.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .hover(|b| b.bg(theme.surface_selected))
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx)))
                                    .child("Choose…"),
                            ),
                    )
                    .children(error.map(|message| {
                        div()
                            .mt(px(8.))
                            .text_size(px(12.))
                            .text_color(theme.error_text)
                            .child(message)
                    })),
            )
            .child(
                div()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(theme.border)
                    .children(progress.map(|text| {
                        div()
                            .flex_1()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(text))
                    }))
                    .child(
                        button("cancel-clone")
                            .border_1()
                            .border_color(theme.border)
                            .hover(|b| b.bg(theme.surface_selected))
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
                            .child("Cancel"),
                    )
                    .child(
                        button("start-clone")
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .bg(theme.accent)
                            .text_color(theme.accent_text)
                            .when(busy || target.is_none(), |b| b.opacity(0.5))
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx)))
                            .child("Clone"),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_name_is_the_last_part_without_dot_git() {
        for (url, name) in [
            ("https://github.com/acme/gitdesk.git", "gitdesk"),
            ("https://github.com/acme/gitdesk", "gitdesk"),
            ("https://github.com/acme/gitdesk/", "gitdesk"),
            ("git@github.com:acme/gitdesk.git", "gitdesk"),
            ("ssh://git@host:2222/acme/gitdesk.git", "gitdesk"),
            ("/tmp/some/repo", "repo"),
            ("  repo.git  ", "repo"),
        ] {
            assert_eq!(repo_name_from_url(url).as_deref(), Some(name), "{url}");
        }
    }

    #[test]
    fn addresses_with_no_usable_name_give_none() {
        for url in [
            "",
            "   ",
            "/",
            "..",
            "https://host/..",
            "https://host/.git",
            "a\\..",
        ] {
            assert_eq!(repo_name_from_url(url), None, "{url:?}");
        }
    }

    #[test]
    fn the_destination_is_the_chosen_folder_plus_the_name() {
        assert_eq!(
            destination(Path::new("/home/me"), "git@host:a/b.git"),
            Some(PathBuf::from("/home/me/b"))
        );
        assert_eq!(destination(Path::new("/home/me"), ""), None);
    }

    mod dialog {
        use std::{cell::Cell, rc::Rc};

        use gpui::{TestAppContext, VisualTestContext};
        use tempfile::TempDir;

        use super::super::*;
        use crate::testutil::git;

        /// A dialog that will clone into a fresh folder, and a flag saying it closed.
        fn open<'a>(
            cx: &'a mut TestAppContext,
            parent: &Path,
        ) -> (
            Entity<RepoState>,
            Entity<CloneDialog>,
            Rc<Cell<bool>>,
            &'a mut VisualTestContext,
        ) {
            cx.update(|cx| {
                gpui_component::init(cx);
                cx.set_global(Theme::dark());
            });
            let state = cx.new(|_| RepoState::default());
            let parent = parent.to_owned();
            let (dialog, cx) = cx.add_window_view({
                let state = state.clone();
                move |window, cx| {
                    let mut dialog = CloneDialog::new(state, window, cx);
                    dialog.parent = parent;
                    dialog
                }
            });
            let closed = Rc::new(Cell::new(false));
            let flag = closed.clone();
            cx.update(|_, cx| {
                cx.subscribe(&dialog, move |_, _: &DismissEvent, _| flag.set(true))
                    .detach();
            });
            (state, dialog, closed, cx)
        }

        fn settle(cx: &mut VisualTestContext) {
            for _ in 0..50 {
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(100));
                cx.run_until_parked();
            }
        }

        #[gpui::test]
        fn a_good_address_clones_opens_the_repo_and_closes(cx: &mut TestAppContext) {
            let remote = TempDir::new().unwrap();
            git(remote.path(), &["init", "-q", "-b", "main"]);
            git(
                remote.path(),
                &["commit", "-q", "--allow-empty", "-m", "base"],
            );
            let parent = TempDir::new().unwrap();
            let (state, _dialog, closed, cx) = open(cx, parent.path());

            cx.simulate_input(remote.path().to_str().unwrap());
            cx.simulate_keystrokes("enter");
            settle(cx);

            let name = remote.path().file_name().unwrap().to_owned();
            assert!(parent.path().join(&name).join(".git").exists());
            state.read_with(cx, |state, _| {
                assert!(state.repo().is_some());
                assert!(state.is_trusted());
            });
            assert!(closed.get(), "the dialog should close after a clone");
        }

        #[gpui::test]
        fn a_bad_address_keeps_the_dialog_open_with_the_reason(cx: &mut TestAppContext) {
            let parent = TempDir::new().unwrap();
            let (state, _dialog, closed, cx) = open(cx, parent.path());

            cx.simulate_input("/definitely/not/a/repo");
            cx.simulate_keystrokes("enter");
            settle(cx);

            assert!(!closed.get());
            state.read_with(cx, |state, _| assert!(state.error().is_some()));
            assert!(state.read_with(cx, |state, _| state.repo().is_none()));
        }

        #[gpui::test]
        fn enter_with_nothing_typed_does_nothing(cx: &mut TestAppContext) {
            let parent = TempDir::new().unwrap();
            let (state, _dialog, closed, cx) = open(cx, parent.path());
            cx.simulate_keystrokes("enter");
            settle(cx);
            assert!(!closed.get());
            state.read_with(cx, |state, _| {
                assert!(state.operation().is_none());
                assert!(state.error().is_none());
            });
        }

        #[gpui::test]
        fn escape_closes_when_idle(cx: &mut TestAppContext) {
            let parent = TempDir::new().unwrap();
            let (_state, _dialog, closed, cx) = open(cx, parent.path());
            cx.simulate_keystrokes("escape");
            assert!(closed.get());
        }
    }
}
