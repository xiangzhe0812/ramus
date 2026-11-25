use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    Action, AnyElement, AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    actions::{CloneRepository, OpenRepository},
    repo_state::RepoState,
    settings::RecentRepository,
    theme::{MONO_FONT, Theme},
};

/// The repository switcher: a filter, the remembered repositories, then Add and Clone.
pub struct RepositoryPopover {
    repo_state: Entity<RepoState>,
    filter: Entity<InputState>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for RepositoryPopover {}

impl Focusable for RepositoryPopover {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Whether `entry` matches what the user typed: every word, in any order, ignoring case,
/// against the repository's name or the folder it opens.
fn matches_filter(entry: &RecentRepository, filter: &str) -> bool {
    let name = entry.name().to_lowercase();
    let path = entry.open_path.to_string_lossy().to_lowercase();
    filter.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        name.contains(&word) || path.contains(&word)
    })
}

/// `path` as the user would write it: the home folder shown as `~`.
fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl RepositoryPopover {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        filter.update(cx, |input, cx| input.focus(window, cx));

        let _subscriptions = vec![
            cx.observe(&repo_state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
                match event {
                    InputEvent::Change => cx.notify(),
                    // Enter opens the first repository the filter leaves.
                    InputEvent::PressEnter { .. } => {
                        if let Some(first) = this.shown(cx).into_iter().next() {
                            this.pick(first, cx);
                        }
                    }
                    _ => {}
                }
            }),
        ];

        Self {
            repo_state,
            filter,
            focus_handle: cx.focus_handle(),
            _subscriptions,
        }
    }

    /// The remembered repositories that match the filter, most recent first.
    fn shown(&self, cx: &gpui::App) -> Vec<RecentRepository> {
        let filter = self.filter.read(cx).value().to_string();
        self.repo_state
            .read(cx)
            .recent_repositories()
            .iter()
            .filter(|entry| matches_filter(entry, &filter))
            .cloned()
            .collect()
    }

    fn pick(&mut self, entry: RecentRepository, cx: &mut Context<Self>) {
        if !entry.open_path.exists() {
            return;
        }
        let busy = self.repo_state.update(cx, |state, cx| {
            if !state.can_switch() {
                return true;
            }
            if !state.is_open_repository(&entry) {
                state.switch_repository(&entry, cx);
            }
            false
        });
        // A switch can't start while git is busy or a folder is opening, so the list stays
        // up rather than closing as if it had worked.
        if !busy {
            cx.emit(DismissEvent);
        }
    }

    fn repository_row(
        &self,
        ix: usize,
        entry: &RecentRepository,
        home: Option<&Path>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.repo_state.read(cx);
        let is_open = state.is_open_repository(entry);
        let missing = !entry.open_path.exists();
        // A folder that is gone can't be opened, and none can while git is busy; such
        // rows are dimmed and not clickable. A gone one can still be removed.
        let can_pick = !missing && state.can_switch();
        let view = cx.entity();
        div()
            .id(("repository", ix))
            .w_full()
            .h(px(theme.metrics.row_height))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .when(is_open, |row| row.bg(theme.surface_selected))
            .when(!can_pick, |row| row.text_color(theme.text_muted))
            .when(can_pick, |row| {
                let view = view.clone();
                let picked = entry.clone();
                row.cursor_pointer()
                    .hover(|row| row.bg(theme.surface_selected))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.pick(picked.clone(), cx))
                    })
            })
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .text_color(theme.accent_soft)
                    .child(if is_open { "✓" } else { "" }),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(160.))
                    .truncate()
                    .child(SharedString::from(entry.name())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO_FONT)
                    .text_size(px(theme.metrics.text_sm))
                    .text_color(theme.text_muted)
                    .child(if missing {
                        SharedString::from("Folder not found")
                    } else {
                        display_path(&entry.open_path, home).into()
                    }),
            )
            .child({
                let forgotten = entry.clone();
                div()
                    .id(("forget-repository", ix))
                    .flex_none()
                    .w(px(20.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|button| button.bg(theme.pill_active).text_color(theme.text))
                    .on_click(move |_, _, cx| {
                        // Only the ✕; the row underneath must not open the repository.
                        cx.stop_propagation();
                        view.update(cx, |this, cx| {
                            this.repo_state
                                .update(cx, |state, cx| state.forget_repository(&forgotten, cx))
                        })
                    })
                    .child("✕")
            })
            .into_any_element()
    }

    fn action_row(
        &self,
        id: &'static str,
        label: &'static str,
        shortcut: &'static str,
        action: Box<dyn Action>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .w_full()
            .h(px(theme.metrics.row_height))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .hover(|row| row.bg(theme.surface_selected))
            .on_click(cx.listener(move |_, _, window, cx| {
                cx.emit(DismissEvent);
                window.dispatch_action(action.boxed_clone(), cx);
            }))
            .child(div().flex_1().child(label))
            .child(
                div()
                    .text_size(px(theme.metrics.text_xs))
                    .text_color(theme.text_muted)
                    .child(shortcut),
            )
    }
}

impl Render for RepositoryPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let shown = self.shown(cx);
        let anything_remembered = !self.repo_state.read(cx).recent_repositories().is_empty();

        let body: AnyElement = if shown.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_muted)
                .child(if anything_remembered {
                    "No repositories match"
                } else {
                    "Repositories you open or clone show here"
                })
                .into_any_element()
        } else {
            let row_theme = theme.clone();
            let home = home();
            uniform_list(
                "repositories",
                shown.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|ix| {
                            let entry = shown.get(ix)?;
                            Some(this.repository_row(ix, entry, home.as_deref(), &row_theme, cx))
                        })
                        .collect()
                }),
            )
            .flex_1()
            .into_any_element()
        };

        div()
            .id("repository-popover")
            .track_focus(&self.focus_handle)
            .w(px(480.))
            .h(px(480.))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(theme.metrics.radius_lg))
            .shadow_lg()
            .overflow_hidden()
            // Clicks inside must not reach the backdrop that closes the popover.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .p(px(12.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Input::new(&self.filter).aria_label("Filter repositories")),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .py(px(4.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(self.action_row(
                        "add-local-repository",
                        "Add local repository…",
                        "⌘ O",
                        Box::new(OpenRepository),
                        &theme,
                        cx,
                    ))
                    .child(self.action_row(
                        "clone-repository",
                        "Clone repository…",
                        "⇧ ⌘ O",
                        Box::new(CloneRepository),
                        &theme,
                        cx,
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(open_path: &str) -> RecentRepository {
        RecentRepository {
            repository: Path::new(open_path).join(".git"),
            open_path: open_path.into(),
        }
    }

    #[test]
    fn the_filter_matches_name_or_path_every_word_any_order() {
        let ramus = entry("/Users/me/orca/ramus");
        assert!(matches_filter(&ramus, ""));
        assert!(matches_filter(&ramus, "RAMUS"));
        assert!(matches_filter(&ramus, "ramus orca"));
        assert!(matches_filter(&ramus, "orca ramus"));
        assert!(!matches_filter(&ramus, "ramus dotfiles"));
    }

    #[test]
    fn the_home_folder_shows_as_a_tilde() {
        let home = Path::new("/Users/me");
        assert_eq!(
            display_path(Path::new("/Users/me/code/ramus"), Some(home)),
            "~/code/ramus"
        );
        assert_eq!(display_path(home, Some(home)), "~");
    }

    #[test]
    fn a_path_outside_home_is_shown_whole() {
        assert_eq!(
            display_path(Path::new("/Volumes/work/api"), Some(Path::new("/Users/me"))),
            "/Volumes/work/api"
        );
        assert_eq!(display_path(Path::new("/srv/x"), None), "/srv/x");
    }
}
