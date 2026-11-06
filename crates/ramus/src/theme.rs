use gpui::{Global, Rgba, rgb};

/// Every color the UI uses. Views read from this struct, never from literals,
/// so adding a light theme is a data-only change.
#[derive(Clone, Debug)]
pub struct Theme {
    pub background: Rgba,
    pub surface: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub tag_background: Rgba,
    pub tag_text: Rgba,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x0d1016),
            surface: rgb(0x151922),
            border: rgb(0x232a36),
            text: rgb(0xe8ebf0),
            accent: rgb(0x3b7be0),
            accent_text: rgb(0xffffff),
            tag_background: rgb(0x2a2036),
            tag_text: rgb(0xd0b4f5),
        }
    }
}

impl Global for Theme {}
