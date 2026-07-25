use crate::theme::Theme;
use averroes_core::config::SetupWizard;
use gpui::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusField {
    ApiKeyEnv,
    Model,
}

pub struct SetupWizardView {
    wizard: SetupWizard,
    theme: Theme,
    saved: bool,
    focused_field: Option<FocusField>,
}

impl SetupWizardView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            wizard: SetupWizard::new(),
            theme: Theme::default(),
            saved: false,
            focused_field: None,
        }
    }

    pub fn is_done(&self) -> bool {
        self.saved
    }

    fn select_provider(&mut self, provider: &str, cx: &mut Context<Self>) {
        self.wizard.provider = provider.to_string();
        self.wizard.model.clear();
        if self.wizard.api_key_env.is_empty() {
            self.wizard.api_key_env = match provider {
                "openai" => "OPENAI_API_KEY".to_string(),
                _ => "ANTHROPIC_API_KEY".to_string(),
            };
        }
        cx.notify();
    }

    fn save_and_continue(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.wizard.save_config() {
            eprintln!("Failed to save config: {}", e);
            return;
        }
        self.saved = true;
        cx.notify();
    }

    fn focus_field(&mut self, field: FocusField, cx: &mut Context<Self>) {
        self.focused_field = Some(field);
        cx.notify();
    }

    fn handle_input_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }

        if let Some(field) = self.focused_field {
            match field {
                FocusField::ApiKeyEnv => {
                    if event.keystroke.key == "backspace" {
                        self.wizard.api_key_env.pop();
                    } else if let Some(ref ch) = event.keystroke.key_char {
                        self.wizard.api_key_env.push_str(ch);
                    }
                }
                FocusField::Model => {
                    if event.keystroke.key == "backspace" {
                        self.wizard.model.pop();
                    } else if let Some(ref ch) = event.keystroke.key_char {
                        self.wizard.model.push_str(ch);
                    }
                }
            }
            cx.notify();
        }
    }
}

impl Render for SetupWizardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.fg)
            .font_family("SF Mono")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_input_key(event, cx);
            }))
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
                                        let provider_name = name.to_lowercase();
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
                                            .hover(|style| {
                                                if !is_active {
                                                    style.bg(theme.border)
                                                } else {
                                                    style
                                                }
                                            })
                                            .id(ElementId::Name(format!("provider-{}", provider_name).into()))
                                            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                                this.select_provider(&provider_name, cx);
                                            }))
                                            .child(*name)
                                    }),
                                ))
                            )
                            .child(div().flex().flex_col().gap_2().child(
                                div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child("API KEY ENV VAR"),
                            ).child({
                                let is_focused = self.focused_field == Some(FocusField::ApiKeyEnv);
                                let display_text = if self.wizard.api_key_env.is_empty() {
                                    "ANTHROPIC_API_KEY".to_string()
                                } else {
                                    self.wizard.api_key_env.clone()
                                };
                                div().bg(theme.bg).rounded_md().border_1().border_color(if is_focused { theme.accent } else { theme.border })
                                    .px_3().py_2().text_sm().text_color(theme.fg)
                                    .cursor_pointer()
                                    .id(ElementId::Name("field-api-key".into()))
                                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                        this.focus_field(FocusField::ApiKeyEnv, cx);
                                    }))
                                    .child(if is_focused {
                                        format!("{}|", display_text)
                                    } else {
                                        display_text
                                    })
                            }))
                            .child(div().flex().flex_col().gap_2().child(
                                div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child("MODEL"),
                            ).child({
                                let is_focused = self.focused_field == Some(FocusField::Model);
                                let display_text = self.wizard.default_model().to_string();
                                div().bg(theme.bg).rounded_md().border_1().border_color(if is_focused { theme.accent } else { theme.border })
                                    .px_3().py_2().text_sm().text_color(theme.fg)
                                    .cursor_pointer()
                                    .id(ElementId::Name("field-model".into()))
                                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                        this.focus_field(FocusField::Model, cx);
                                    }))
                                    .child(if is_focused {
                                        format!("{}|", display_text)
                                    } else {
                                        display_text
                                    })
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_end()
                                    .child(
                                        div()
                                            .px_6()
                                            .py_2()
                                            .bg(if self.saved { theme.success } else { theme.accent })
                                            .text_color(theme.bg)
                                            .rounded_md()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .cursor_pointer()
                                            .id(ElementId::Name("btn-save".into()))
                                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                                this.save_and_continue(cx);
                                            }))
                                            .child(if self.saved { "Saved!" } else { "Save & Start" }),
                                    ),
                            ),
                    ),
            )
    }
}
