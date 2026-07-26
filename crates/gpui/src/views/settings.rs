use crate::ui::theme::UiTheme;
use crate::ui::{
    button, field_label, field_surface, panel, provider_card, provider_card_title,
    render_text_with_cursor, status_badge, utf16_range_to_byte_range, ButtonVariant,
    TextSelection,
};
use averroes_core::config::{AppConfig, SetupWizard};
use gpui::*;
use std::ops::Range;

#[derive(Debug, Clone)]
pub enum SettingsEvent {
    Saved,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusField {
    ApiKeyEnv,
    Model,
}

pub struct SettingsView {
    config: AppConfig,
    wizard: SetupWizard,
    saved: bool,
    focused_field: Option<FocusField>,
    error: Option<String>,
    focus_handle: FocusHandle,
    input_selection: TextSelection,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl EntityInputHandler for SettingsView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let value = self.focused_value()?;
        let range = utf16_range_to_byte_range(value, &range_utf16);
        adjusted_range.replace(range_utf16);
        value.get(range).map(str::to_string)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(
            self.input_selection
                .selected_text_range(self.focused_value()?),
        )
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.input_selection
            .marked_text_range(self.focused_value()?)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.input_selection.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.focused_field {
            Some(FocusField::ApiKeyEnv) => {
                self.input_selection
                    .replace_text(&mut self.wizard.api_key_env, range_utf16, text)
            }
            Some(FocusField::Model) => {
                self.input_selection
                    .replace_text(&mut self.wizard.model, range_utf16, text)
            }
            None => return,
        }
        self.saved = false;
        self.error = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.focused_field {
            Some(FocusField::ApiKeyEnv) => self.input_selection.replace_marked_text(
                &mut self.wizard.api_key_env,
                range_utf16,
                text,
                new_selected_range,
            ),
            Some(FocusField::Model) => self.input_selection.replace_marked_text(
                &mut self.wizard.model,
                range_utf16,
                text,
                new_selected_range,
            ),
            None => return,
        }
        self.saved = false;
        self.error = None;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>, config: AppConfig) -> Self {
        Self {
            config: config.clone(),
            wizard: SetupWizard::from_config(&config),
            saved: false,
            focused_field: None,
            error: None,
            focus_handle: cx.focus_handle(),
            input_selection: TextSelection::default(),
        }
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.saved = false;
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    fn select_provider(&mut self, provider: &str, cx: &mut Context<Self>) {
        self.wizard.select_provider(provider);
        self.saved = false;
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.wizard.api_key_env.trim().is_empty() {
            self.wizard.api_key_env = match self.wizard.provider.as_str() {
                "openai" => "OPENAI_API_KEY".into(),
                _ => "ANTHROPIC_API_KEY".into(),
            };
        }
        if self.wizard.api_key_env.trim().is_empty()
            || self.wizard.default_model().trim().is_empty()
        {
            self.saved = false;
            self.error = Some("Environment variable and model are required.".into());
            cx.notify();
            return;
        }
        let mut config = self.config.clone();
        self.wizard.apply_to_config(&mut config);
        match config.save() {
            Ok(()) => {
                self.config = config;
                self.saved = true;
                self.error = None;
                cx.emit(SettingsEvent::Saved);
            }
            Err(error) => {
                self.saved = false;
                self.error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    fn focus_field(&mut self, field: FocusField, window: &mut Window, cx: &mut Context<Self>) {
        self.focused_field = Some(field);
        if matches!(field, FocusField::ApiKeyEnv) && self.wizard.api_key_env.is_empty() {
            self.wizard.api_key_env = match self.wizard.provider.as_str() {
                "openai" | "generic" => "OPENAI_API_KEY",
                _ => "ANTHROPIC_API_KEY",
            }
            .into();
        }
        if matches!(field, FocusField::Model) && self.wizard.model.is_empty() {
            self.wizard.model = self.wizard.default_model().into();
        }
        let cursor = match field {
            FocusField::ApiKeyEnv => self.wizard.api_key_env.len(),
            FocusField::Model => self.wizard.model.len(),
        };
        self.input_selection.set_cursor(cursor);
        self.input_selection.unmark();
        self.error = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn focused_value(&self) -> Option<&str> {
        match self.focused_field {
            Some(FocusField::ApiKeyEnv) => Some(&self.wizard.api_key_env),
            Some(FocusField::Model) => Some(&self.wizard.model),
            None => None,
        }
    }

    fn handle_input_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" || event.keystroke.key == "enter" {
            self.focused_field = None;
            window.blur();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if (modifiers.control || modifiers.platform)
            && !modifiers.alt
            && !modifiers.function
            && event.keystroke.key == "a"
        {
            match self.focused_field {
                Some(FocusField::ApiKeyEnv) => {
                    self.input_selection.select_all(&self.wizard.api_key_env)
                }
                Some(FocusField::Model) => self.input_selection.select_all(&self.wizard.model),
                None => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }

        let handled = match (self.focused_field, event.keystroke.key.as_str()) {
            (Some(FocusField::ApiKeyEnv), "backspace") => {
                self.input_selection.backspace(&mut self.wizard.api_key_env);
                true
            }
            (Some(FocusField::Model), "backspace") => {
                self.input_selection.backspace(&mut self.wizard.model);
                true
            }
            (Some(FocusField::ApiKeyEnv), "delete") => {
                self.input_selection.delete(&mut self.wizard.api_key_env);
                true
            }
            (Some(FocusField::Model), "delete") => {
                self.input_selection.delete(&mut self.wizard.model);
                true
            }
            (Some(FocusField::ApiKeyEnv), "left") => {
                self.input_selection
                    .move_left(&self.wizard.api_key_env, modifiers.shift);
                true
            }
            (Some(FocusField::Model), "left") => {
                self.input_selection
                    .move_left(&self.wizard.model, modifiers.shift);
                true
            }
            (Some(FocusField::ApiKeyEnv), "right") => {
                self.input_selection
                    .move_right(&self.wizard.api_key_env, modifiers.shift);
                true
            }
            (Some(FocusField::Model), "right") => {
                self.input_selection
                    .move_right(&self.wizard.model, modifiers.shift);
                true
            }
            (Some(FocusField::ApiKeyEnv), "home") => {
                self.input_selection.move_home(modifiers.shift);
                true
            }
            (Some(FocusField::Model), "home") => {
                self.input_selection.move_home(modifiers.shift);
                true
            }
            (Some(FocusField::ApiKeyEnv), "end") => {
                self.input_selection
                    .move_end(&self.wizard.api_key_env, modifiers.shift);
                true
            }
            (Some(FocusField::Model), "end") => {
                self.input_selection
                    .move_end(&self.wizard.model, modifiers.shift);
                true
            }
            (Some(field), "c")
                if (modifiers.control || modifiers.platform)
                    && !modifiers.alt
                    && !modifiers.function =>
            {
                let text = match field {
                    FocusField::ApiKeyEnv => &self.wizard.api_key_env,
                    FocusField::Model => &self.wizard.model,
                };
                if !self.input_selection.range.is_empty() {
                    let selected = text[self.input_selection.range.clone()].to_string();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(selected));
                }
                true
            }
            (Some(field), "x")
                if (modifiers.control || modifiers.platform)
                    && !modifiers.alt
                    && !modifiers.function =>
            {
                if !self.input_selection.range.is_empty() {
                    let value = match field {
                        FocusField::ApiKeyEnv => &mut self.wizard.api_key_env,
                        FocusField::Model => &mut self.wizard.model,
                    };
                    let selected = value[self.input_selection.range.clone()].to_string();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(selected));
                    self.input_selection.replace_text(value, None, "");
                }
                true
            }
            (Some(field), "v")
                if (modifiers.control || modifiers.platform)
                    && !modifiers.alt
                    && !modifiers.function =>
            {
                if let Some(text) = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text().map(|t| t.replace('\n', " ")))
                {
                    match field {
                        FocusField::ApiKeyEnv => self.input_selection.replace_text(
                            &mut self.wizard.api_key_env,
                            None,
                            &text,
                        ),
                        FocusField::Model => self
                            .input_selection
                            .replace_text(&mut self.wizard.model, None, &text),
                    }
                }
                true
            }
            (Some(field), key)
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                if let Some(ref ch) = event.keystroke.key_char {
                    match field {
                        FocusField::ApiKeyEnv => self.input_selection.replace_text(
                            &mut self.wizard.api_key_env,
                            None,
                            ch,
                        ),
                        FocusField::Model => self.input_selection.replace_text(
                            &mut self.wizard.model,
                            None,
                            ch,
                        ),
                    }
                    true
                } else if key.len() == 1 {
                    let ch = if modifiers.shift {
                        key.to_uppercase()
                    } else {
                        key.to_string()
                    };
                    match field {
                        FocusField::ApiKeyEnv => self.input_selection.replace_text(
                            &mut self.wizard.api_key_env,
                            None,
                            &ch,
                        ),
                        FocusField::Model => self.input_selection.replace_text(
                            &mut self.wizard.model,
                            None,
                            &ch,
                        ),
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if handled {
            self.saved = false;
            self.error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::light();
        let provider = self.wizard.provider.clone();
        let env_value = if self.wizard.api_key_env.is_empty() {
            match provider.as_str() {
                "openai" | "generic" => "OPENAI_API_KEY".to_string(),
                _ => "ANTHROPIC_API_KEY".to_string(),
            }
        } else {
            self.wizard.api_key_env.clone()
        };
        let model_value = self.wizard.default_model().to_string();
        let env_available = std::env::var(&env_value)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        let save_label = if self.saved { "Saved" } else { "Save changes" };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_input_key(event, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .p(px(28.0))
                    .child(
                        div()
                            .font(UiTheme::display_font())
                            .font_weight(FontWeight::BOLD)
                            .text_2xl()
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Provider and model settings are shared with the CLI."),
                    ),
            )
            .child(
                panel(theme)
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .mx(px(28.0))
                    .max_w(px(620.0))
                    .p(px(24.0))
                    .child(field_label(theme, "Provider"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(10.0))
                            .child({
                                let selected = provider == "anthropic";
                                provider_card(theme, selected)
                                    .flex_1()
                                    .id(ElementId::Name("settings-provider-anthropic".into()))
                                    .on_click(cx.listener(
                                        |this, _event: &ClickEvent, _window, cx| {
                                            this.select_provider("anthropic", cx);
                                        },
                                    ))
                                    .child(provider_card_title(theme, "Anthropic"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Claude models"),
                                    )
                            })
                            .child({
                                let selected = provider == "openai";
                                provider_card(theme, selected)
                                    .flex_1()
                                    .id(ElementId::Name("settings-provider-openai".into()))
                                    .on_click(cx.listener(
                                        |this, _event: &ClickEvent, _window, cx| {
                                            this.select_provider("openai", cx);
                                        },
                                    ))
                                    .child(provider_card_title(theme, "OpenAI"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("GPT models"),
                                    )
                            })
                            .child({
                                let selected = provider == "generic";
                                provider_card(theme, selected)
                                    .flex_1()
                                    .id(ElementId::Name("settings-provider-generic".into()))
                                    .on_click(cx.listener(
                                        |this, _event: &ClickEvent, _window, cx| {
                                            this.select_provider("generic", cx);
                                        },
                                    ))
                                    .child(provider_card_title(theme, "Generic"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("OpenAI-compatible"),
                                    )
                            }),
                    )
                    .child(field_label(theme, "API key environment variable"))
                    .child({
                        let focused = self.focused_field == Some(FocusField::ApiKeyEnv);
                        field_surface(theme, focused, false)
                            .id(ElementId::Name("settings-api-key-env".into()))
                            .cursor(CursorStyle::IBeam)
                            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                                this.focus_field(FocusField::ApiKeyEnv, window, cx);
                            }))
                            .child(if focused {
                                render_text_with_cursor(
                                    &self.wizard.api_key_env,
                                    &self.input_selection,
                                )
                            } else {
                                env_value
                            })
                    })
                    .child(status_badge(
                        theme,
                        if env_available {
                            "API key available"
                        } else {
                            "API key missing"
                        },
                    ))
                    .child(field_label(theme, "Default model"))
                    .child({
                        let focused = self.focused_field == Some(FocusField::Model);
                        field_surface(theme, focused, false)
                            .id(ElementId::Name("settings-model".into()))
                            .cursor(CursorStyle::IBeam)
                            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                                this.focus_field(FocusField::Model, window, cx);
                            }))
                            .child(if focused {
                                render_text_with_cursor(&self.wizard.model, &self.input_selection)
                            } else {
                                model_value
                            })
                    })
                    .child(if let Some(error) = self.error.clone() {
                        div().text_xs().text_color(theme.destructive).child(error)
                    } else {
                        div()
                    })
                    .child(
                        div().flex().justify_end().child(
                            button(theme, ButtonVariant::Primary, save_label)
                                .id(ElementId::Name("settings-save".into()))
                                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                    this.save(cx);
                                })),
                        ),
                    ),
            )
    }
}
