mod theme;
mod title_bar;
mod workspace;

use gpui::{Bounds, WindowBounds, WindowOptions, prelude::*, px, size};

use crate::{theme::Theme, workspace::Workspace};

fn main() {
    gpui_platform::application().run(|cx| {
        cx.set_global(Theme::dark());
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(Workspace::new),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
