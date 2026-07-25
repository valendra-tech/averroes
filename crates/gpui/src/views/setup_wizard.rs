use crate::theme::Theme;
use averroes_core::config::SetupWizard;
use gpui::*;

pub struct SetupWizardView {
    wizard: SetupWizard,
    theme: Theme,
    saved: bool,
}

impl SetupWizardView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            wizard: SetupWizard::new(),
            theme: Theme::default(),
            saved: false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.saved
    }

    pub fn to_config(&self) -> averroes_core::config::AppConfig {
        self.wizard.to_config()
    }
}

impl Render for SetupWizardView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.fg)
            .font_family("SF Mono")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .justify_center()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w(px(420.0))
                            .bg(theme.surface)
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .p_8()
                            .gap_6()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.accent)
                                    .child("Welcome to Averroes"),
                            )
                            .child(
                                div().text_sm().text_color(theme.muted)
                                    .child("Configure your AI provider to get started."),
                            )
                            .child(div().flex().flex_col().gap_2().child(
                                div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child("PROVIDER"),
                            ).child(
                                div().flex().flex_row().gap_2().children(
                                    ["Anthropic", "OpenAI"].iter().map(|name| {
                                        let is_active = self.wizard.provider
                                            == name.to_lowercase();
                                        div()
                                            .px_4()
                                            .py_2()
                                            .rounded_md()
                                            .bg(if is_active { theme.accent } else { theme.surface })
                                            .text_color(if is_active { theme.bg } else { theme.fg })
                                            .text_sm()
                                            .cursor_pointer()
                                            .border_1()
                                            .border_color(if is_active { theme.accent } else { theme.border })
                                            .child(*name)
                                    }),
                                ))
                            )
                            .child(div().flex().flex_col().gap_2().child(
                                div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child("API KEY ENV VAR"),
                            ).child(
                                div().bg(theme.bg).rounded_md().border_1().border_color(theme.border)
                                    .px_3().py_2().text_sm().text_color(theme.fg)
                                    .child(if self.wizard.api_key_env.is_empty() {
                                        "ANTHROPIC_API_KEY".to_string()
                                    } else {
                                        self.wizard.api_key_env.clone()
                                    }),
                            ))
                            .child(div().flex().flex_col().gap_2().child(
                                div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child("MODEL"),
                            ).child(
                                div().bg(theme.bg).rounded_md().border_1().border_color(theme.border)
                                    .px_3().py_2().text_sm().text_color(theme.fg)
                                    .child(self.wizard.default_model().to_string()),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_end()
                                    .child(
                                        div()
                                            .px_6()
                                            .py_2()
                                            .bg(theme.accent)
                                            .text_color(theme.bg)
                                            .rounded_md()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .cursor_pointer()
                                            .child("Save & Start"),
                                    ),
                            ),
                    ),
            )
    }
}
