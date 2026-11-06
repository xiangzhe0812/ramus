use gpui::{Global, Rgba, rgb};

/// Monospace font for branch names, hashes and code. Replace with a bundled
/// JetBrains Mono once fonts are embedded.
pub const MONO_FONT: &str = "Menlo";

/// A badge's background and foreground.
#[derive(Clone, Copy, Debug)]
pub struct Badge {
    pub background: Rgba,
    pub text: Rgba,
}

/// Every color the UI uses. Views read from this struct, never from literals,
/// so adding a light theme is a data-only change.
#[derive(Clone, Debug)]
pub struct Theme {
    pub background: Rgba,
    pub surface: Rgba,
    pub surface_selected: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub pill_active: Rgba,
    pub modified: Badge,
    pub added: Badge,
    pub deleted: Badge,
    pub renamed: Badge,
    pub conflicted: Badge,
    pub error_background: Rgba,
    pub error_border: Rgba,
    pub error_text: Rgba,
}

impl Theme {
    pub fn dark() -> Self {
        let badge = |background, text| Badge {
            background: rgb(background),
            text: rgb(text),
        };
        Self {
            background: rgb(0x0d1016),
            surface: rgb(0x151922),
            surface_selected: rgb(0x1d2431),
            border: rgb(0x232a36),
            text: rgb(0xe8ebf0),
            text_muted: rgb(0x98a2b3),
            accent: rgb(0x3b7be0),
            accent_text: rgb(0xffffff),
            pill_active: rgb(0x262f40),
            modified: badge(0x3a3020, 0xf0c878),
            added: badge(0x153327, 0x86e0b4),
            deleted: badge(0x40242a, 0xf59aa2),
            renamed: badge(0x1b2a45, 0x9bc0ff),
            conflicted: badge(0x40242a, 0xf59aa2),
            error_background: rgb(0x40242a),
            error_border: rgb(0x6b3640),
            error_text: rgb(0xfbe3e5),
        }
    }
}

impl Global for Theme {}
