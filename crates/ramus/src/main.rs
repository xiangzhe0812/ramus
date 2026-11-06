mod actions;
mod branch_popover;
mod changes_view;
mod component_theme;
mod diff_rows;
mod repo_state;
mod settings;
mod theme;
mod title_bar;
mod welcome;
mod workspace;

use gpui::{AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};

use crate::{
    actions::{
        DiscardSelected, OpenRepository, SelectNextFile, SelectPreviousFile, ToggleBranches,
        ToggleSelectedFile,
    },
    repo_state::RepoState,
    theme::Theme,
    workspace::Workspace,
};

fn main() {
    gpui_platform::application().run(|cx| {
        gpui_component::init(cx);
        let theme = Theme::dark();
        component_theme::apply(&theme, cx);
        cx.set_global(theme);
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenRepository, None),
            KeyBinding::new("cmd-b", ToggleBranches, None),
            KeyBinding::new("cmd-backspace", DiscardSelected, Some("Workspace")),
            KeyBinding::new("down", SelectNextFile, Some("ChangesList")),
            KeyBinding::new("up", SelectPreviousFile, Some("ChangesList")),
            KeyBinding::new("space", ToggleSelectedFile, Some("ChangesList")),
        ]);

        let repo_state = cx.new(|_| RepoState::persistent());
        if let Some(path) = std::env::args_os().nth(1) {
            repo_state.update(cx, |state, cx| state.open(path.into(), cx));
        }

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(repo_state, window, cx));
                cx.new(|cx| gpui_component::Root::new(workspace, window, cx))
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
