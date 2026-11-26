use gpui::{Global, Rgba, WindowAppearance, rgb, rgba};

use crate::settings::Appearance;

/// Monospace font for branch names, hashes and code, bundled (see `assets.rs`).
pub const MONO_FONT: &str = "JetBrains Mono";

/// The interface font, bundled; the window's default, and for text inside an element that
/// is otherwise monospace.
pub const UI_FONT: &str = "Inter";

/// The title bar's height. A constant, not only a metric, because the positions of the
/// popovers hanging under it and of the window's traffic lights are constants too.
pub const TITLE_BAR_HEIGHT: f32 = 44.;

/// Named sizes, in pixels. Views use these rather than literals, so the type scale,
/// corners and density can change in one place.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// Badges, shortcuts, small labels.
    pub text_xs: f32,
    /// Secondary lines, paths, code.
    pub text_sm: f32,
    /// Body text: the window's default.
    pub text_base: f32,
    /// Dialog titles.
    pub text_lg: f32,
    /// The Welcome heading.
    pub text_title: f32,
    /// Chips and small marks.
    pub radius_sm: f32,
    /// Pills, buttons, inputs, rows.
    pub radius_md: f32,
    /// Panels, popovers, dialogs.
    pub radius_lg: f32,
    /// Pills, buttons and tabs.
    pub control_height: f32,
    /// One-line list rows. The title bar's height is `TITLE_BAR_HEIGHT`.
    pub row_height: f32,
}

/// A badge's background and foreground.
#[derive(Clone, Copy, Debug)]
pub struct Badge {
    pub background: Rgba,
    pub text: Rgba,
}

/// Every color the UI uses. Views read from this struct, never from literals.
#[derive(Clone, Debug)]
pub struct Theme {
    /// A dark palette, so the text-input library is put in its dark mode too.
    dark: bool,
    pub background: Rgba,
    pub surface: Rgba,
    pub surface_selected: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    /// Main buttons, ticked checkboxes and picked options.
    pub accent: Rgba,
    pub accent_text: Rgba,
    /// The focus ring and text selection.
    pub focus: Rgba,
    /// A lighter accent for text links and small actions on a dark surface.
    pub accent_soft: Rgba,
    pub pill_active: Rgba,
    /// Dims the window behind a dialog.
    pub scrim: Rgba,
    /// A heavier dim, behind a confirmation that interrupts what the user was doing.
    pub scrim_strong: Rgba,
    /// The soft shadow under a panel.
    pub panel_shadow: Rgba,
    /// A count badge on an accent-coloured button.
    pub accent_chip: Rgba,
    pub modified: Badge,
    pub added: Badge,
    pub deleted: Badge,
    pub renamed: Badge,
    pub conflicted: Badge,
    pub diff_added_background: Rgba,
    pub diff_added_sign: Rgba,
    pub diff_added_text: Rgba,
    pub diff_removed_background: Rgba,
    pub diff_removed_sign: Rgba,
    pub diff_removed_text: Rgba,
    pub diff_hunk_background: Rgba,
    pub diff_hunk_text: Rgba,
    pub diff_line_number: Rgba,
    pub diff_context_text: Rgba,
    pub syntax_keyword: Rgba,
    pub syntax_string: Rgba,
    pub syntax_type: Rgba,
    pub syntax_function: Rgba,
    pub syntax_comment: Rgba,
    pub error_background: Rgba,
    pub error_border: Rgba,
    pub error_text: Rgba,
    pub metrics: Metrics,
}

fn badge(background: u32, text: u32) -> Badge {
    Badge {
        background: rgb(background),
        text: rgb(text),
    }
}

/// The sizes, which don't change with the palette.
const METRICS: Metrics = Metrics {
    text_xs: 11.,
    text_sm: 12.,
    text_base: 13.,
    text_lg: 15.,
    text_title: 20.,
    radius_sm: 4.,
    radius_md: 6.,
    radius_lg: 8.,
    control_height: 28.,
    row_height: 32.,
};

impl Theme {
    /// The theme for the user's `choice`, given what macOS is using.
    pub fn resolve(choice: Appearance, system: WindowAppearance) -> Self {
        let dark = match choice {
            Appearance::Light => false,
            Appearance::Dark => true,
            Appearance::System => matches!(
                system,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        };
        if dark { Self::dark() } else { Self::light() }
    }

    pub fn is_dark(&self) -> bool {
        self.dark
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            background: rgb(0x000000),
            surface: rgb(0x0a0c0e),
            surface_selected: rgb(0x1f252a),
            border: rgb(0x1f252a),
            text: rgb(0xe6e8eb),
            text_muted: rgb(0x8a919a),
            accent: rgb(0xe6e8eb),
            accent_text: rgb(0x0a0c0e),
            focus: rgb(0x3b7be0),
            accent_soft: rgb(0x6aa5ff),
            pill_active: rgb(0x262c33),
            scrim: rgba(0x00000066),
            scrim_strong: rgba(0x00000099),
            panel_shadow: rgba(0x00000059),
            accent_chip: rgba(0x0000001f),
            modified: badge(0x3a3020, 0xf0c878),
            added: badge(0x153327, 0x86e0b4),
            deleted: badge(0x40242a, 0xf59aa2),
            renamed: badge(0x1b2a45, 0x9bc0ff),
            conflicted: badge(0x40242a, 0xf59aa2),
            diff_added_background: rgba(0x4cc38a21),
            diff_added_sign: rgb(0x4cc38a),
            diff_added_text: rgb(0xe1f5ea),
            diff_removed_background: rgba(0xf0707a21),
            diff_removed_sign: rgb(0xf0707a),
            diff_removed_text: rgb(0xfbe3e5),
            diff_hunk_background: rgb(0x10151b),
            diff_hunk_text: rgb(0x9bc0ff),
            diff_line_number: rgb(0x8b95a1),
            diff_context_text: rgb(0xd5d9de),
            syntax_keyword: rgb(0xc9a9f7),
            syntax_string: rgb(0x9be0b0),
            syntax_type: rgb(0xf0c878),
            syntax_function: rgb(0x8fb4ee),
            syntax_comment: rgb(0x7d8796),
            error_background: rgb(0x40242a),
            error_border: rgb(0x6b3640),
            error_text: rgb(0xfbe3e5),
            metrics: METRICS,
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            background: rgb(0xffffff),
            surface: rgb(0xf6f6f7),
            surface_selected: rgb(0xe8e8ea),
            border: rgb(0xe2e2e5),
            text: rgb(0x1b1d20),
            text_muted: rgb(0x6a7078),
            accent: rgb(0x1b1d20),
            accent_text: rgb(0xffffff),
            focus: rgb(0x2f6fd6),
            accent_soft: rgb(0x2563c9),
            pill_active: rgb(0xe4e4e7),
            scrim: rgba(0x00000033),
            scrim_strong: rgba(0x00000059),
            panel_shadow: rgba(0x00000026),
            accent_chip: rgba(0xffffff33),
            modified: badge(0xfbf0d9, 0x8a5a00),
            added: badge(0xdcf3e6, 0x1d7a4a),
            deleted: badge(0xfbe1e3, 0xb4303c),
            renamed: badge(0xe0eafc, 0x2557b0),
            conflicted: badge(0xfbe1e3, 0xb4303c),
            diff_added_background: rgba(0x2ea04326),
            diff_added_sign: rgb(0x1f8a4c),
            diff_added_text: rgb(0x14321f),
            diff_removed_background: rgba(0xe5484d1f),
            diff_removed_sign: rgb(0xc93c45),
            diff_removed_text: rgb(0x3d1418),
            diff_hunk_background: rgb(0xf0f3f8),
            diff_hunk_text: rgb(0x2557b0),
            diff_line_number: rgb(0x8a9099),
            diff_context_text: rgb(0x2b2f33),
            syntax_keyword: rgb(0x7a3fc4),
            syntax_string: rgb(0x1f7a45),
            syntax_type: rgb(0x9a6200),
            syntax_function: rgb(0x2759b8),
            syntax_comment: rgb(0x687079),
            error_background: rgb(0xfde8ea),
            error_border: rgb(0xf3b8bf),
            error_text: rgb(0x8f1d2a),
            metrics: METRICS,
        }
    }
}

impl Global for Theme {}

#[cfg(test)]
mod tests {
    use gpui::WindowAppearance;

    use super::*;
    use crate::settings::Appearance;

    /// The spread between a colour's strongest and weakest channel: 0 for a pure grey.
    fn tint(color: Rgba) -> f32 {
        let channels = [color.r, color.g, color.b];
        channels.iter().cloned().fold(0., f32::max) - channels.iter().cloned().fold(1., f32::min)
    }

    #[test]
    fn the_background_is_black() {
        assert_eq!(Theme::dark().background, rgb(0x000000));
    }

    #[test]
    fn resolve_follows_macos_when_the_choice_is_system() {
        for (system, dark) in [
            (WindowAppearance::Light, false),
            (WindowAppearance::VibrantLight, false),
            (WindowAppearance::Dark, true),
            (WindowAppearance::VibrantDark, true),
        ] {
            assert_eq!(
                Theme::resolve(Appearance::System, system).is_dark(),
                dark,
                "{system:?}"
            );
        }
    }

    #[test]
    fn a_forced_choice_ignores_macos() {
        assert!(!Theme::resolve(Appearance::Light, WindowAppearance::Dark).is_dark());
        assert!(Theme::resolve(Appearance::Dark, WindowAppearance::Light).is_dark());
    }

    /// WCAG relative luminance of an opaque colour.
    fn luminance(color: Rgba) -> f32 {
        let linear = |c: f32| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    /// `color` laid over the opaque `under`, for the translucent diff backgrounds.
    fn over(color: Rgba, under: Rgba) -> Rgba {
        let mix = |top: f32, bottom: f32| top * color.a + bottom * (1. - color.a);
        Rgba {
            r: mix(color.r, under.r),
            g: mix(color.g, under.g),
            b: mix(color.b, under.b),
            a: 1.,
        }
    }

    fn contrast(text: Rgba, background: Rgba) -> f32 {
        let (a, b) = (luminance(text), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn both_themes_keep_text_readable() {
        for theme in [Theme::dark(), Theme::light()] {
            let page = theme.background;
            let mut pairs = vec![
                ("text on background", theme.text, page),
                ("text on surface", theme.text, theme.surface),
                ("muted on background", theme.text_muted, page),
                ("muted on surface", theme.text_muted, theme.surface),
                ("button text", theme.accent_text, theme.accent),
                ("link", theme.accent_soft, theme.surface),
                (
                    "added line",
                    theme.diff_added_text,
                    over(theme.diff_added_background, page),
                ),
                (
                    "removed line",
                    theme.diff_removed_text,
                    over(theme.diff_removed_background, page),
                ),
                ("context line", theme.diff_context_text, page),
                (
                    "hunk header",
                    theme.diff_hunk_text,
                    theme.diff_hunk_background,
                ),
                ("error", theme.error_text, theme.error_background),
                ("keyword", theme.syntax_keyword, page),
                ("string", theme.syntax_string, page),
                ("type", theme.syntax_type, page),
                ("function", theme.syntax_function, page),
                ("comment", theme.syntax_comment, page),
            ];
            for (name, badge) in [
                ("modified", theme.modified),
                ("added", theme.added),
                ("deleted", theme.deleted),
                ("renamed", theme.renamed),
                ("conflicted", theme.conflicted),
            ] {
                pairs.push((name, badge.text, badge.background));
            }
            for (name, text, background) in pairs {
                let ratio = contrast(text, background);
                assert!(
                    ratio >= 4.5,
                    "{} theme: {name} is {ratio:.2}:1",
                    if theme.is_dark() { "dark" } else { "light" }
                );
            }
        }
    }

    #[test]
    fn the_chrome_has_no_blue_tint() {
        for theme in [Theme::dark(), Theme::light()] {
            for (name, color) in [
                ("background", theme.background),
                ("surface", theme.surface),
                ("surface_selected", theme.surface_selected),
                ("border", theme.border),
                ("pill_active", theme.pill_active),
            ] {
                // Orca's own greys carry a faint cool cast (#1f252a spreads by 11); no more.
                assert!(tint(color) <= 14. / 255., "{name} is tinted: {color:?}");
            }
        }
    }

    #[test]
    fn views_use_named_text_sizes_and_radii() {
        // Every source file but this one, so a new view is covered too.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut literals = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            if !file.ends_with(".rs") || file == "theme.rs" {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for (number, line) in source.lines().enumerate() {
                let literal = |call: &str| {
                    line.split(call)
                        .skip(1)
                        .any(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
                };
                if literal("text_size(px(") || literal("rounded(px(") {
                    literals.push(format!("{file}:{}: {}", number + 1, line.trim()));
                }
            }
        }
        assert!(
            literals.is_empty(),
            "use theme.metrics instead of:\n{}",
            literals.join("\n")
        );
    }

    /// How light a colour looks, 0 (black) to 1 (white).
    fn lightness(color: Rgba) -> f32 {
        0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
    }

    #[test]
    fn main_buttons_are_light_and_neutral_with_dark_text() {
        let theme = Theme::dark();
        assert!(lightness(theme.accent) > 0.8, "{:?}", theme.accent);
        assert!(tint(theme.accent) <= 14. / 255.);
        assert!(
            lightness(theme.accent_text) < 0.1,
            "{:?}",
            theme.accent_text
        );
    }

    #[test]
    fn focus_stays_blue() {
        let focus = Theme::dark().focus;
        assert!(focus.b > focus.r && focus.b > focus.g);
    }

    #[test]
    fn sizes_step_up() {
        let m = Theme::dark().metrics;
        assert!(m.text_xs < m.text_sm && m.text_sm < m.text_base);
        assert!(m.text_base < m.text_lg && m.text_lg < m.text_title);
        assert!(m.radius_sm < m.radius_md && m.radius_md < m.radius_lg);
        assert!(m.control_height < m.row_height && m.row_height < TITLE_BAR_HEIGHT);
    }
}
