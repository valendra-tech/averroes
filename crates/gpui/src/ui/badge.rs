use super::theme::UiTheme;
use gpui::*;

pub fn status_badge(theme: UiTheme, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_subtle)
        .text_color(theme.primary)
        .font(UiTheme::mono_font())
        .font_weight(FontWeight::MEDIUM)
        .text_xs()
        .px(px(8.0))
        .py(px(2.0))
        .child(text.into())
}
