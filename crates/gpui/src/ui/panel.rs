use super::theme::UiTheme;
use gpui::{div, px, Div, Pixels, Styled};

pub fn panel(theme: UiTheme) -> Div {
    div()
        .bg(theme.card)
        .text_color(theme.foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(px(UiTheme::RADIUS))
        .font_family(UiTheme::UI_FONT)
}

pub fn panel_with_padding(theme: UiTheme, padding: Pixels) -> Div {
    panel(theme).p(padding)
}
