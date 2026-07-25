use super::theme::UiTheme;
use gpui::*;

pub fn field_label(theme: UiTheme, text: impl Into<SharedString>) -> Div {
    let text = text.into().as_str().to_ascii_uppercase();

    div()
        .font_family(UiTheme::MONO_FONT)
        .font_weight(FontWeight::MEDIUM)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text)
}

pub fn field_surface(theme: UiTheme, focused: bool, invalid: bool) -> Div {
    let border = if invalid {
        theme.destructive
    } else if focused {
        theme.brand_magenta
    } else {
        theme.border
    };

    div()
        .bg(theme.card)
        .border_1()
        .border_color(border)
        .rounded(px(UiTheme::RADIUS))
        .font_family(UiTheme::UI_FONT)
        .px(px(12.0))
        .py(px(8.0))
}
