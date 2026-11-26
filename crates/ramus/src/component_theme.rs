//! Maps Ramus's theme onto gpui-component, so its text inputs match the rest of the UI.

use gpui::{App, Hsla, px};
use gpui_component::ThemeMode;

use crate::theme::{MONO_FONT, Theme, UI_FONT};

pub fn apply(theme: &Theme, cx: &mut App) {
    // Switching modes reloads the library's colors, so set the mode before overriding them.
    let mode = if theme.is_dark() {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    gpui_component::Theme::change(mode, None, cx);

    let component = gpui_component::Theme::global_mut(cx);
    let colors = &mut component.colors;
    colors.background = theme.background.into();
    colors.foreground = theme.text.into();
    colors.muted_foreground = theme.text_muted.into();
    colors.border = theme.border.into();
    colors.input = theme.border.into();
    colors.caret = theme.text.into();
    colors.ring = theme.focus.into();
    colors.primary = theme.focus.into();
    colors.selection = Hsla::from(theme.focus).opacity(0.35);
    component.font_family = UI_FONT.into();
    component.mono_font_family = MONO_FONT.into();
    component.font_size = px(theme.metrics.text_base);
    component.radius = px(theme.metrics.radius_md);
    component.radius_lg = px(theme.metrics.radius_lg);
}
