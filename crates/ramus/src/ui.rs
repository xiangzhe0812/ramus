//! Small pieces of UI shared by several views.

use std::path::Path;

use gpui::{BoxShadow, Div, Rgba, Styled, Svg, div, point, px, svg};

use crate::theme::Theme;

/// One of the rounded panels the views are made of: surface colour, hairline border, and the
/// soft drop shadow the design gives them.
pub fn panel(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .rounded(px(theme.metrics.radius_lg))
        .shadow(vec![BoxShadow {
            color: theme.panel_shadow.into(),
            offset: point(px(0.), px(8.)),
            blur_radius: px(24.),
            spread_radius: px(0.),
            inset: false,
        }])
        .overflow_hidden()
}

/// One of the bundled icons (`"folder"`, `"worktree"`, `"branch"`, `"search"`), drawn in `color`.
pub fn icon(name: &str, size: f32, color: Rgba) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// `path` as the user would write it: the home folder shortened to `~`. Compared by path
/// component, so `/Users/zhe2/x` is not mistaken for something inside `/Users/zhe`.
pub fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::abbreviate_home;

    #[test]
    fn the_home_folder_becomes_a_tilde() {
        let home = Some(Path::new("/Users/zhe"));
        assert_eq!(
            abbreviate_home(Path::new("/Users/zhe/dev/ramus"), home),
            "~/dev/ramus"
        );
        assert_eq!(abbreviate_home(Path::new("/Users/zhe"), home), "~");
    }

    #[test]
    fn a_folder_that_only_shares_a_prefix_is_left_alone() {
        let home = Some(Path::new("/Users/zhe"));
        assert_eq!(
            abbreviate_home(Path::new("/Users/zhe2/x"), home),
            "/Users/zhe2/x"
        );
        assert_eq!(abbreviate_home(Path::new("/tmp/x"), home), "/tmp/x");
    }

    #[test]
    fn without_a_home_nothing_is_shortened() {
        assert_eq!(
            abbreviate_home(Path::new("/Users/zhe/x"), None),
            "/Users/zhe/x"
        );
    }
}
