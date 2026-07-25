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
        .font_family(UiTheme::DISPLAY_FONT)
        .cursor_pointer()
        .p(px(16.0))
}

#[cfg(test)]
mod tests {
    use super::provider_card;
    use super::super::theme::UiTheme;
    use gpui::Styled;

    #[test]
    fn provider_card_uses_display_font() {
        let mut card = provider_card(UiTheme::light(), true);

        assert_eq!(
            card.style()
                .text
                .as_ref()
                .and_then(|text| text.font_family.as_ref())
                .map(|font| font.as_str()),
            Some(UiTheme::DISPLAY_FONT),
        );
    }
}
