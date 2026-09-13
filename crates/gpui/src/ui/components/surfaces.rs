use crate::ui::tokens;
use crate::ui::UiTheme;
use gpui::{div, px, Div, ParentElement, SharedString, Styled};

pub fn card(theme: UiTheme, padding: f32) -> Div {
    div()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.hairline)
        .rounded(px(tokens::RADIUS_CARD))
        .p(px(padding))
}

pub fn badge(theme: UiTheme, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(theme.hairline)
        .bg(theme.surface_subtle)
        .text_color(theme.muted)
        .text_size(px(tokens::TEXT_CAPTION))
        .font_weight(tokens::WEIGHT_MEDIUM)
        .px(px(tokens::SPACE_8))
        .py(px(tokens::SPACE_2))
        .child(text.into())
}

pub fn empty_state(theme: UiTheme, title: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(tokens::SPACE_8))
        .px(px(tokens::SPACE_16))
        .py(px(tokens::SPACE_24))
        .child(
            div()
                .text_size(px(tokens::TEXT_TITLE3))
                .font_weight(tokens::WEIGHT_SEMIBOLD)
                .text_color(theme.foreground)
                .child(title.into()),
        )
}

pub fn field_label(theme: UiTheme, label: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(tokens::TEXT_SMALL))
        .font_weight(tokens::WEIGHT_MEDIUM)
        .text_color(theme.foreground)
        .child(label.into())
}

pub fn field_hint(theme: UiTheme, hint: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(tokens::TEXT_CAPTION))
        .text_color(theme.muted)
        .child(hint.into())
}
