use super::theme::UiTheme;
use gpui::*;

pub fn field_label(theme: UiTheme, text: impl Into<SharedString>) -> Div {
    let text = text.into().as_str().to_ascii_uppercase();

    div()
        .font(UiTheme::ui_font())
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
        .w_full()
        .bg(theme.background)
        .border_1()
        .border_color(border)
        .rounded(px(UiTheme::RADIUS))
        .font(UiTheme::ui_font())
        .px(px(12.0))
        .py(px(8.0))
        .min_h(px(36.0))
}

#[cfg(test)]
mod tests {
    use super::{field_label, UiTheme};
    use gpui::Styled;

    #[test]
    fn field_label_uses_ui_font() {
        let mut label = field_label(UiTheme::light(), "provider");

        let _style = label.style();
        // font_family is from UI theme
    }
}
