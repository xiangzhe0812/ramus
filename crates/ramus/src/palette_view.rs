use gpui::{
    AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{
    palette::{Entry, Situation, Target, entries, move_selection, row_positions},
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    title_bar::primary_sync,
};

/// The palette closing because the user picked something to run.
pub struct RunEntry(pub Target);

/// The command palette: one box to find a command or a branch.
pub struct PaletteView {
    repo_state: Entity<RepoState>,
    query: Entity<InputState>,
    selected: usize,
    /// The list scrolls when there are more rows than room.
    list_scroll: ScrollHandle,
    /// The highlighted row moved, so bring it into view on the next render.
    scroll_to_selection: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for PaletteView {}
impl EventEmitter<RunEntry> for PaletteView {}

impl Focusable for PaletteView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PaletteView {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Branches are only listed once something is typed, but load them now so they
        // are there by then.
        repo_state.update(cx, |state, cx| state.load_branches(cx));

        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search commands and branches"));
        query.update(cx, |input, cx| input.focus(window, cx));

        let _subscriptions = vec![
            cx.observe(&repo_state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
                match event {
                    InputEvent::Change => {
                        // A new query makes the old position meaningless.
                        this.selected = 0;
                        this.scroll_to_selection = true;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.run_selected(cx),
                    _ => {}
                }
            }),
        ];

        Self {
            repo_state,
            query,
            selected: 0,
            list_scroll: ScrollHandle::new(),
            scroll_to_selection: false,
            focus_handle: cx.focus_handle(),
            _subscriptions,
        }
    }

    /// The entries for what is typed right now.
    fn current_entries(&self, cx: &gpui::App) -> Vec<Entry> {
        let state = self.repo_state.read(cx);
        let branches = state.branches();
        let situation = Situation {
            has_repo: state.repo().is_some(),
            busy: state.is_busy(),
            primary_sync: state
                .status()
                .and_then(primary_sync)
                .filter(|_| state.has_remote())
                .map(|action| (action.kind, action.label)),
            local: branches.map_or(&[], |list| list.local.as_slice()),
            remote_only: branches.map_or(&[], |list| list.remote_only.as_slice()),
        };
        entries(&situation, self.query.read(cx).value().as_ref())
    }

    fn run(&mut self, target: Target, cx: &mut Context<Self>) {
        cx.emit(RunEntry(target));
        cx.emit(DismissEvent);
    }

    fn run_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.current_entries(cx).into_iter().nth(self.selected) {
            self.run(entry.target, cx);
        }
    }

    fn move_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.current_entries(cx).len();
        self.selected = move_selection(self.selected, delta, len);
        self.scroll_to_selection = true;
        cx.notify();
    }
}

fn section_title(title: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .h(px(28.))
        .px(px(16.))
        .flex()
        .items_center()
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(title)
}

impl Render for PaletteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let found = self.current_entries(cx);
        let selected = self.selected.min(found.len().saturating_sub(1));

        // Scroll the highlighted entry into view; the first entry also shows its title.
        if std::mem::take(&mut self.scroll_to_selection) {
            let rows = row_positions(&found);
            let row = if selected == 0 {
                0
            } else {
                rows.get(selected).copied().unwrap_or(0)
            };
            self.list_scroll.scroll_to_item(row);
        }

        let mut list = div()
            .id("palette-list")
            .flex_1()
            .min_h_0()
            .py(px(6.))
            .flex()
            .flex_col()
            // Clip and scroll: more rows than room must not spill over the footer.
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);
        if found.is_empty() {
            list = list.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.text_muted)
                    .child("Nothing matches"),
            );
        }
        let mut shown_branch_title = false;
        for (ix, entry) in found.iter().enumerate() {
            if ix == 0 && !entry.is_branch() {
                list = list.child(section_title("COMMANDS", &theme));
            }
            if entry.is_branch() && !shown_branch_title {
                shown_branch_title = true;
                list = list.child(section_title("BRANCHES", &theme));
            }
            let view = cx.entity();
            let target = entry.target.clone();
            list = list.child(
                div()
                    .id(("entry", ix))
                    .h(px(44.))
                    .px(px(16.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(12.))
                    .cursor_pointer()
                    .when(ix == selected, |row| row.bg(theme.surface_selected))
                    .hover(|row| row.bg(theme.surface_selected))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.run(target.clone(), cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(entry.is_branch(), |label| label.font_family(MONO_FONT))
                            .child(SharedString::from(entry.label.clone())),
                    )
                    .children(entry.shortcut.map(|keys| key_chip(keys, &theme)))
                    .when(entry.is_branch(), |row| {
                        row.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child("Switch"),
                        )
                    }),
            );
        }

        div()
            .id("palette")
            .track_focus(&self.focus_handle)
            .w(px(680.))
            .h(px(460.))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            // Clicks inside must not reach the backdrop that closes the palette.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => cx.emit(DismissEvent),
                    "down" => this.move_by(1, cx),
                    "up" => this.move_by(-1, cx),
                    _ => {}
                }
            }))
            .child(
                div()
                    .h(px(52.))
                    .flex_none()
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.query).aria_label("Search commands and branches"),
                        ),
                    )
                    .child(key_chip("Esc", &theme)),
            )
            .child(list)
            .child(
                div()
                    .h(px(36.))
                    .flex_none()
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child("Up/Down to move")
                    .child("Enter to run"),
            )
    }
}

fn key_chip(keys: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .px(px(6.))
        .py(px(2.))
        .border_1()
        .border_color(theme.border)
        .rounded(px(4.))
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(keys)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::palette::Command;

    fn open(cx: &mut TestAppContext) -> (Entity<PaletteView>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let state = cx.new(|_| RepoState::default());
        let (palette, cx) = cx.add_window_view(|window, cx| PaletteView::new(state, window, cx));
        cx.run_until_parked();
        (palette, cx)
    }

    /// What the palette ran, as it is told to the workspace.
    fn record_runs(
        palette: &Entity<PaletteView>,
        cx: &mut VisualTestContext,
    ) -> Rc<RefCell<Vec<Target>>> {
        let ran = Rc::new(RefCell::new(Vec::new()));
        let sink = ran.clone();
        cx.update(|_, cx| {
            cx.subscribe(palette, move |_, event: &RunEntry, _| {
                sink.borrow_mut().push(event.0.clone())
            })
            .detach();
        });
        ran
    }

    #[gpui::test]
    fn enter_runs_the_first_entry(cx: &mut TestAppContext) {
        let (palette, cx) = open(cx);
        let ran = record_runs(&palette, cx);
        cx.simulate_keystrokes("enter");
        assert_eq!(*ran.borrow(), [Target::Command(Command::OpenRepository)]);
    }

    #[gpui::test]
    fn typing_narrows_the_list_and_enter_runs_what_is_left(cx: &mut TestAppContext) {
        let (palette, cx) = open(cx);
        let ran = record_runs(&palette, cx);
        // With no repository open, "open" is the only command; "zzz" matches nothing.
        cx.simulate_input("zzz");
        cx.simulate_keystrokes("enter");
        assert!(ran.borrow().is_empty());
        assert!(palette.read_with(cx, |p, cx| p.current_entries(cx).is_empty()));
    }

    #[gpui::test]
    fn arrow_keys_move_the_selection_even_while_the_input_has_focus(cx: &mut TestAppContext) {
        let dir = tempfile::TempDir::new().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=T", "-c", "user.email=t@e.com"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);

        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        let (palette, cx) = cx.add_window_view(|window, cx| PaletteView::new(state, window, cx));
        cx.run_until_parked();
        let ran = record_runs(&palette, cx);

        cx.simulate_keystrokes("down down");
        assert_eq!(palette.read_with(cx, |p, _| p.selected), 2);
        cx.simulate_keystrokes("up");
        assert_eq!(palette.read_with(cx, |p, _| p.selected), 1);
        // Up from the top wraps to the last entry.
        cx.simulate_keystrokes("up up");
        let last = palette.read_with(cx, |p, cx| p.current_entries(cx).len() - 1);
        assert_eq!(palette.read_with(cx, |p, _| p.selected), last);

        // Enter runs the highlighted entry, not the first.
        cx.simulate_keystrokes("down down enter");
        assert_eq!(*ran.borrow(), [Target::Command(Command::CloneRepository)]);
    }

    #[gpui::test]
    fn escape_dismisses(cx: &mut TestAppContext) {
        let (palette, cx) = open(cx);
        let dismissed = Rc::new(RefCell::new(false));
        let flag = dismissed.clone();
        cx.update(|_, cx| {
            cx.subscribe(&palette, move |_, _: &DismissEvent, _| {
                *flag.borrow_mut() = true
            })
            .detach();
        });
        cx.simulate_keystrokes("escape");
        assert!(*dismissed.borrow());
    }
}
