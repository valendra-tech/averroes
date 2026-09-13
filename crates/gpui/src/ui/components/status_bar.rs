use crate::ui::tokens;
use crate::ui::UiTheme;
use gpui::{div, px, Div, Styled};

pub fn status_bar_surface(theme: UiTheme) -> Div {
    div()
        .flex_none()
        .h(px(tokens::CONTROL_REGULAR))
        .px(px(tokens::SPACE_12))
        .flex()
        .items_center()
        .justify_between()
        .border_t_1()
        .border_color(theme.hairline)
        .bg(theme.rail)
        .text_size(px(tokens::TEXT_CAPTION))
        .text_color(theme.faint)
}
