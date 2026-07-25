use super::theme::UiTheme;
use gpui::*;

pub fn provider_card(theme: UiTheme, selected: bool) -> Div {
    let (background, border) = if selected {
        (theme.accent, theme.primary)
    } else {
        (theme.card, theme.border)
    };

    div()
        .flex()
        .flex_col()
        .bg(background)
        .border_1()
        .border_color(border)
        .rounded(px(UiTheme::RADIUS))
        .font_family(UiTheme::UI_FONT)
        .cursor_pointer()
        .p(px(16.0))
}
