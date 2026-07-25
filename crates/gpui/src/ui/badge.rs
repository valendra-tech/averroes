use super::theme::UiTheme;
use gpui::*;

pub fn status_badge(theme: UiTheme, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .rounded(px(UiTheme::RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(theme.accent)
        .text_color(theme.foreground)
        .font_family(UiTheme::MONO_FONT)
        .font_weight(FontWeight::MEDIUM)
        .text_xs()
        .px(px(8.0))
        .py(px(2.0))
        .child(text.into())
}
