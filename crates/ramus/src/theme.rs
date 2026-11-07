use gpui::{Global, Rgba, rgb, rgba};

/// Monospace font for branch names, hashes and code. Replace with a bundled
/// JetBrains Mono once fonts are embedded.
pub const MONO_FONT: &str = "Menlo";

/// The platform UI font, for text inside an element that is otherwise monospace.
pub const UI_FONT: &str = ".SystemUIFont";

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
            accent_soft: rgb(0x6aa5ff),
            pill_active: rgb(0x262f40),
            scrim: rgba(0x00000066),
            scrim_strong: rgba(0x00000099),
            panel_shadow: rgba(0x00000059),
            accent_chip: rgba(0xffffff38),
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
            diff_hunk_background: rgb(0x182236),
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
        }
    }
}

impl Global for Theme {}
