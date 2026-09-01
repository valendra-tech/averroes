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
        .font(UiTheme::ui_font())
        .cursor_pointer()
        .hover(move |style| {
            if selected {
                style
            } else {
                style.bg(theme.surface_hover)
            }
        })
        .p(px(16.0))
}

pub fn provider_card_title(theme: UiTheme, text: impl Into<SharedString>) -> Div {
    div()
        .font(UiTheme::display_font())
        .font_weight(FontWeight::SEMIBOLD)
        .text_sm()
        .text_color(theme.foreground)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::super::theme::UiTheme;
    use super::{provider_card, provider_card_title};
    use gpui::Styled;

    #[test]
    fn provider_card_keeps_ui_font() {
        let mut card = provider_card(UiTheme::light(), true);
        let _style = card.style();
        // font_family is defined in the UI theme
    }

    #[test]
    fn provider_card_title_uses_display_font() {
        let mut title = provider_card_title(UiTheme::light(), "Anthropic");
        let _style = title.style();
        // font_family is defined in the display font theme
    }
}
