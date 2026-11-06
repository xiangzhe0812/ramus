mod actions;
mod changes_view;
mod diff_rows;
mod repo_state;
mod theme;
mod title_bar;
mod welcome;
mod workspace;

use gpui::{AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};

use crate::{actions::OpenRepository, repo_state::RepoState, theme::Theme, workspace::Workspace};

fn main() {
    gpui_platform::application().run(|cx| {
        cx.set_global(Theme::dark());
        cx.bind_keys([KeyBinding::new("cmd-o", OpenRepository, None)]);

        let repo_state = cx.new(|_| RepoState::default());
        if let Some(path) = std::env::args_os().nth(1) {
            repo_state.update(cx, |state, cx| state.open(path.into(), cx));
        }

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Workspace::new(repo_state, window, cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
