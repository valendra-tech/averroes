use crate::ui::theme::UiTheme;
use crate::ui::{
    button, field_label, field_surface, panel, provider_card, provider_card_title,
    render_text_with_cursor, utf16_range_to_byte_range, ButtonVariant, TextSelection,
};
use averroes_core::config::{AppConfig, SetupWizard};
use gpui::*;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusField {
    ApiKeyEnv,
    Model,
}

#[derive(Debug, Clone)]
pub struct SetupWizardSaved;

pub struct SetupWizardView {
    base_config: AppConfig,
    wizard: SetupWizard,
    saved: bool,
    focused_field: Option<FocusField>,
    error: Option<String>,
    saving: bool,
    focus_handle: FocusHandle,
    input_selection: TextSelection,
    save_task: Option<Task<()>>,
}

impl EventEmitter<SetupWizardSaved> for SetupWizardView {}

impl EntityInputHandler for SetupWizardView {
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

impl SetupWizardView {
    pub fn new(cx: &mut Context<Self>, base_config: AppConfig) -> Self {
        Self {
            wizard: SetupWizard::from_config(&base_config),
            base_config,
            saved: false,
            focused_field: None,
            error: None,
            saving: false,
            focus_handle: cx.focus_handle(),
            input_selection: TextSelection::default(),
            save_task: None,
        }
    }

    fn select_provider(&mut self, provider: &str, cx: &mut Context<Self>) {
        self.wizard.select_provider(provider);
        self.error = None;
        cx.notify();
    }

    fn save_and_continue(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }

        let mut config = self.base_config.clone();
        self.wizard.apply_to_config(&mut config);
        self.saving = true;
        self.error = None;
        let task_config = config.clone();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let result = task_config.save();
            _ = this.update(cx, |view, cx| {
                view.saving = false;
                view.save_task = None;
                match result {
                    Ok(()) => {
                        view.base_config = config;
                        view.saved = true;
                        view.error = None;
                        cx.emit(SetupWizardSaved);
                    }
                    Err(error) => {
                        view.saved = false;
                        view.error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
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
        self.focus_handle.focus(window, cx);
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
            window.blur(cx);
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

impl Focusable for SetupWizardView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SetupWizardView {
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
        let can_save = !env_value.trim().is_empty() && !model_value.trim().is_empty();
        let save_label = if self.saving {
            "Saving..."
        } else if self.saved {
            "Saved"
        } else {
            "Save & start"
        };

        let save_button = if can_save && !self.saving {
            button(theme, ButtonVariant::Primary, save_label)
                .id(ElementId::Name("setup-save".into()))
                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                    this.save_and_continue(cx);
                }))
                .into_any_element()
        } else {
            button(theme, ButtonVariant::Secondary, save_label)
                .id(ElementId::Name("setup-save".into()))
                .text_color(theme.muted_foreground)
                .cursor(CursorStyle::Arrow)
                .into_any_element()
        };

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
                    .flex_row()
                    .items_center()
                    .h(px(44.0))
                    .px(px(18.0))
                    .bg(theme.card)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font(UiTheme::display_font())
                            .font_weight(FontWeight::BOLD)
                            .text_lg()
                            .child("Averroes"),
                    )
                    .child(
                        div()
                            .ml(px(12.0))
                            .text_xs()
                            .font(UiTheme::mono_font())
                            .text_color(theme.muted_foreground)
                            .child("SETUP"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(32.0))
                    .p(px(32.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w(px(300.0))
                            .gap(px(14.0))
                            .child(
                                div()
                                    .font(UiTheme::display_font())
                                    .font_weight(FontWeight::BOLD)
                                    .text_2xl()
                                    .child("Build your workspace."),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("Choose a provider and connect Averroes to your existing API key environment variable."),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font(UiTheme::mono_font())
                                    .text_color(theme.primary)
                                    .child("Configuration is shared with Averroes frontends."),
                            ),
                    )
                    .child(
                        panel(theme)
                            .flex()
                            .flex_col()
                            .w(px(420.0))
                            .gap(px(16.0))
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
                                            .id(ElementId::Name("setup-provider-anthropic".into()))
                                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                                this.select_provider("anthropic", cx);
                                            }))
                                            .child(provider_card_title(theme, "Anthropic"))
                                            .child(div().text_xs().text_color(theme.muted_foreground).child("Claude models"))
                                    })
                                    .child({
                                        let selected = provider == "openai";
                                        provider_card(theme, selected)
                                            .flex_1()
                                            .id(ElementId::Name("setup-provider-openai".into()))
                                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                                this.select_provider("openai", cx);
                                            }))
                                            .child(provider_card_title(theme, "OpenAI"))
                                            .child(div().text_xs().text_color(theme.muted_foreground).child("GPT models"))
                                    })
                                    .child({
                                        let selected = provider == "generic";
                                        provider_card(theme, selected)
                                            .flex_1()
                                            .id(ElementId::Name("setup-provider-generic".into()))
                                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                                this.select_provider("generic", cx);
                                            }))
                                            .child(provider_card_title(theme, "Generic"))
                                            .child(div().text_xs().text_color(theme.muted_foreground).child("OpenAI-compatible"))
                                    }),
                            )
                            .child(field_label(theme, "API key environment variable"))
                            .child({
                                let focused = self.focused_field == Some(FocusField::ApiKeyEnv);
                                field_surface(theme, focused, false)
                                    .id(ElementId::Name("setup-api-key-env".into()))
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
                            .child(field_label(theme, "Default model"))
                            .child({
                                let focused = self.focused_field == Some(FocusField::Model);
                                field_surface(theme, focused, false)
                                    .id(ElementId::Name("setup-model".into()))
                                    .cursor(CursorStyle::IBeam)
                                            .on_click(cx.listener(|this, _event: &ClickEvent, window, cx| {
                                                this.focus_field(FocusField::Model, window, cx);
                                    }))
                                    .child(if focused {
                                        render_text_with_cursor(
                                            &self.wizard.model,
                                            &self.input_selection,
                                        )
                                    } else {
                                        model_value
                                    })
                            })
                            .child(if let Some(error) = self.error.clone() {
                                div()
                                    .text_xs()
                                    .text_color(theme.destructive)
                                    .child(error)
                            } else {
                                div()
                            })
                            .child(div().flex().justify_end().child(save_button)),
                    ),
            )
    }
}
