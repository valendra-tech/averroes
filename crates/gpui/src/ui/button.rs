use super::theme::UiTheme;
use gpui::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

pub fn button(theme: UiTheme, variant: ButtonVariant, label: impl Into<SharedString>) -> Div {
    let transparent = rgba(0x00000000);
    let (background, border, foreground, hover_background) = match variant {
        ButtonVariant::Primary => (theme.primary, theme.primary, theme.card, theme.brand_coral),
        ButtonVariant::Secondary => (theme.card, theme.border, theme.foreground, theme.accent),
        ButtonVariant::Ghost => (transparent, transparent, theme.foreground, theme.accent),
        ButtonVariant::Danger => (
            theme.destructive,
            theme.destructive,
            theme.card,
            theme.brand_coral,
        ),
    };

    div()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(UiTheme::RADIUS))
        .border_1()
        .border_color(border)
        .bg(background)
        .text_color(foreground)
        .font(UiTheme::ui_font())
        .font_weight(FontWeight::MEDIUM)
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(hover_background))
        .px(px(12.0))
        .py(px(8.0))
        .child(label.into())
}
