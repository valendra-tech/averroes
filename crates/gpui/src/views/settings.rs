use crate::theme::Theme;
use gpui::*;

pub struct SettingsView {
    pub provider: String,
    pub model: String,
    theme: Theme,
}

impl SettingsView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            theme: Theme::default(),
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .p_6()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.fg)
                    .child("Settings"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted)
                            .child("Provider"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.fg)
                            .child(self.provider.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted)
                            .child("Model"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.fg)
                            .child(self.model.clone()),
                    ),
            )
    }
}
