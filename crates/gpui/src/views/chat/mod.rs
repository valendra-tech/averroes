pub mod message;
mod read_only_message;
mod model_search;

use crate::runtime::AgentFactory;
use crate::session::SessionId;
use crate::ui::composer::{composer_surface, ComposerMode, ComposerState, EffortLevel};
use crate::ui::theme::UiTheme;
use crate::ui::{
    arrow_up, button, chevron_down_icon, plus_icon, provider_logo, spinner, text_field_element,
    utf16_range_to_byte_range, ButtonVariant, TextField,
};
use averroes_core::agent::{Agent, AgentStreamEvent};
use averroes_core::connection::SessionBinding;
use averroes_core::provider::types::ChatMessage;
use averroes_core::provider::{filter_models, merge_catalog, provider_label, ModelInfo};
use averroes_core::session::SessionStore;
use averroes_core::workspace::{WorkspaceConfig, WorkspaceStore};
use gpui::prelude::*;
use gpui::*;
use gpui_component::text::TextView;
use message::MessageBubble;
use model_search::ModelSearchState;
use read_only_message::ReadOnlyMessage;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum ChatViewEvent {
    Submitted { session_id: SessionId, text: String },
    WorkspaceChanged { session_id: SessionId, workspace_id: String },
    ModelChanged(String),
}

pub struct ChatView {
    session_id: SessionId,
    messages: Vec<MessageBubble>,
    composer: ComposerState,
    selected_model: String,
    theme: UiTheme,
    agent: Option<Arc<Agent>>,
    factory: Arc<AgentFactory>,
    store: SessionStore,
    workspace_root: PathBuf,
    pub(crate) workspace_id: Option<String>,
    workspaces: Vec<WorkspaceConfig>,
    workspace_selector_open: bool,
    scroll_handle: ScrollHandle,
    focus_handle: FocusHandle,
    focus_on_render: bool,
    agent_task: Option<Task<()>>,
    attachment_menu_open: bool,
    mode_menu_open: bool,
    model_menu_open: bool,
    effort_menu_open: bool,
    model_catalog: Vec<ModelInfo>,
    model_search: Entity<ModelSearchState>,
    pending_reasoning: String,
    reasoning_effort: Option<String>,
    reasoning_menu_open: bool,
    read_only_message: Entity<ReadOnlyMessage>,
    last_layout: Option<Vec<ShapedLine>>,
    last_line_height: Option<Pixels>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    context_menu_open: bool,
    context_menu_pos: Point<Pixels>,
    message_context_menu_open: bool,
    message_context_menu_pos: Point<Pixels>,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl EntityInputHandler for ChatView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_range_to_byte_range(&self.composer.text, &range_utf16);
        adjusted_range.replace(range_utf16);
        self.composer.text.get(range).map(str::to_string)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(
            self.composer
                .selection
                .selected_text_range(&self.composer.text),
        )
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.composer
            .selection
            .marked_text_range(&self.composer.text)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.composer.selection.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.replace_text_utf16(range_utf16, text);
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
        self.composer
            .replace_marked_text(range_utf16, text, new_selected_range);
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

impl ChatView {
    pub fn new(
        cx: &mut Context<Self>,
        session_id: SessionId,
        agent: Option<Arc<Agent>>,
        factory: Arc<AgentFactory>,
        workspace_root: PathBuf,
        workspace_id: Option<String>,
        workspaces: Vec<WorkspaceConfig>,
        selected_model_override: Option<String>,
        reasoning_effort_override: Option<String>,
    ) -> Self {
        let store = SessionStore::with_dir(workspace_root.join(".averroes").join("sessions"));
        let persisted: Vec<MessageBubble> = store
            .load(session_id.as_str())
            .unwrap_or_default()
            .into_iter()
            .map(MessageBubble::from)
            .collect();

        let mut messages: Vec<MessageBubble> = Vec::new();
        messages.extend(persisted);

        let selected_model = selected_model_override
            .or_else(|| WorkspaceStore::load_last_model())
            .unwrap_or_else(|| factory.provider.default_model().to_string());
        let provider_name = factory.provider.provider_name().to_string();
        let model_catalog = merge_catalog(&provider_name, &selected_model, &[]);
        let model_search = cx.new(|cx| ModelSearchState::new(cx.focus_handle()));
        let read_only_message = cx.new(|cx| ReadOnlyMessage::new(cx.focus_handle()));
        let reasoning_effort = reasoning_effort_override
            .or_else(|| WorkspaceStore::load_last_reasoning_effort());

        cx.observe(&model_search, |_this, _model_search, cx| {
            cx.notify();
        });

        if let Some(ref effort) = reasoning_effort {
            if let Some(ref agent) = agent {
                agent.set_reasoning_effort(Some(effort.clone()));
            }
        }

        Self {
            session_id,
            messages,
            composer: ComposerState::default(),
            selected_model,
            theme: UiTheme::light(),
            agent,
            factory,
            store,
            workspace_root,
            workspace_id,
            workspaces,
            workspace_selector_open: false,
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            focus_on_render: false,
            agent_task: None,
            attachment_menu_open: false,
            mode_menu_open: false,
            model_menu_open: false,
            effort_menu_open: false,
            model_catalog,
            model_search,
            pending_reasoning: String::new(),
            reasoning_effort,
            reasoning_menu_open: false,
            read_only_message,
            last_layout: None,
            last_line_height: None,
            last_bounds: None,
            is_selecting: false,
            scroll_bounds: None,
            context_menu_open: false,
            context_menu_pos: point(px(0.0), px(0.0)),
            message_context_menu_open: false,
            message_context_menu_pos: point(px(0.0), px(0.0)),
        }
    }

    pub(crate) fn save_messages(&self) {
        let core_messages: Vec<ChatMessage> = self
            .messages
            .iter()
            .map(|bubble| match bubble.role {
                message::MessageRole::User => ChatMessage::user(bubble.content.clone()),
                message::MessageRole::Assistant => ChatMessage::assistant(bubble.content.clone()),
                message::MessageRole::System | message::MessageRole::Error => {
                    ChatMessage::system(bubble.content.clone())
                }
            })
            .collect();
        let _ = self.store.save(
            self.session_id.as_str(),
            &core_messages,
            self.workspace_id.as_deref(),
            self.reasoning_effort.as_deref(),
            &SessionBinding::default(),
        );
    }

    fn append_stream_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        if let Some(message) = self
            .messages
            .last_mut()
            .filter(|message| message.role == message::MessageRole::Assistant)
        {
            message.content.push_str(text);
        } else {
            self.messages.push(MessageBubble::assistant(text));
        }
        self.scroll_handle.scroll_to_bottom();
    }

    fn append_stream_text_with_reasoning(&mut self, text: &str, reasoning: Option<&str>) {
        let has_existing = self
            .messages
            .last()
            .map(|m| m.role == message::MessageRole::Assistant)
            .unwrap_or(false);

        if !has_existing {
            let reasoning_str = reasoning.unwrap_or("");
            self.messages
                .push(MessageBubble::assistant_with_reasoning(text, reasoning_str));
        } else if let Some(msg) = self.messages.last_mut() {
            if let Some(r) = reasoning {
                let current = msg.reasoning.take().unwrap_or_default();
                msg.reasoning = Some(format!("{}{}", current, r));
            }
            if !text.is_empty() {
                msg.content.push_str(text);
            }
        }
        self.scroll_handle.scroll_to_bottom();
    }

    fn complete_stream_response(&mut self, response: String) {
        if response.is_empty() {
            return;
        }

        if let Some(message) = self
            .messages
            .last_mut()
            .filter(|message| message.role == message::MessageRole::Assistant)
        {
            message.content = response;
        } else {
            self.messages.push(MessageBubble::assistant(response));
        }
        self.save_messages();
        self.scroll_handle.scroll_to_bottom();
    }

    fn update_last_read_only(&mut self, cx: &mut Context<Self>) {
        let content = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == message::MessageRole::Assistant)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        self.read_only_message.update(cx, |rom, cx| {
            rom.set_content(content);
            cx.notify();
        });
    }

    pub fn request_composer_focus(&mut self, cx: &mut Context<Self>) {
        self.focus_on_render = true;
        cx.notify();
    }

    pub fn reconfigure_agent(
        &mut self,
        provider: Arc<dyn averroes_core::provider::Provider>,
        model: String,
        factory: Arc<AgentFactory>,
    ) {
        let selected_model = model.clone();
        if let Some(agent) = self.agent.as_ref().cloned() {
            agent.reconfigure_provider(provider, model, Arc::clone(&factory.governor));
        }
        self.selected_model = selected_model;
        self.factory = factory;
    }

    pub fn is_processing(&self) -> bool {
        self.composer.processing
    }

    fn element_id(&self, prefix: &str) -> ElementId {
        ElementId::Name(format!("{prefix}-{}", self.session_id).into())
    }

    fn on_composer_click(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.standard_click() {
            return;
        }
        self.composer.focused = true;
        self.context_menu_open = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn on_composer_context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Right {
            return;
        }
        self.context_menu_pos = context_menu_position(event.position, self.last_bounds, window);
        self.context_menu_open = true;
        cx.notify();
    }

    fn on_composer_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        if self.context_menu_open {
            return;
        }
        self.is_selecting = true;
        let pos = self.index_for_position(event.position);
        self.composer
            .select_for_click(pos, event.click_count, event.modifiers.shift);
        if event.click_count >= 2 {
            self.is_selecting = false;
        }
        cx.notify();
    }

    fn on_composer_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = false;
        cx.notify();
    }

    fn on_composer_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.composer.select_to(self.index_for_position(event.position));
            cx.notify();
        }
    }

    fn index_for_position(&self, pos: Point<Pixels>) -> usize {
        if self.composer.text.is_empty() { return 0; }
        let (Some(bounds), Some(lines), Some(lh)) = (
            self.last_bounds.as_ref(), self.last_layout.as_ref(), self.last_line_height,
        ) else { return 0; };
        if pos.y < bounds.top() { return 0; }
        if pos.y >= bounds.bottom() { return self.composer.text.len(); }
        let line_idx = (((pos.y - bounds.top()) / lh) as usize).min(lines.len().saturating_sub(1));
        let mut char_offset = 0usize;
        for (i, line) in lines.iter().enumerate() {
            if i == line_idx {
                return (char_offset + line.closest_index_for_x(pos.x - bounds.left())).min(self.composer.text.len());
            }
            char_offset += line.len();
        }
        self.composer.text.len()
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_menu_open {
            return;
        }

        self.composer.focused = true;
        self.focus_handle.focus(window, cx);

        let modifiers = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "escape" => {
                self.composer.focused = false;
                self.context_menu_open = false;
                window.blur(cx);
                cx.stop_propagation();
                cx.notify();
            }
            "enter"
                if modifiers.shift
                    && !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                // GPUI 0.2.2 text field doesn't support newlines; use space instead
                self.composer
                    .selection
                    .replace_text(&mut self.composer.text, None, "\n");
                cx.stop_propagation();
                cx.notify();
            }
            "enter"
                if !modifiers.shift
                    && !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                cx.stop_propagation();
                self.send_current_message(cx);
            }
            "backspace"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                self.composer.selection.backspace(&mut self.composer.text);
                cx.stop_propagation();
                cx.notify();
            }
            "delete"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                self.composer.selection.delete(&mut self.composer.text);
                cx.stop_propagation();
                cx.notify();
            }
            "left"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                self.composer
                    .selection
                    .move_left(&self.composer.text, modifiers.shift);
                cx.stop_propagation();
                cx.notify();
            }
            "right"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                self.composer
                    .selection
                    .move_right(&self.composer.text, modifiers.shift);
                cx.stop_propagation();
                cx.notify();
            }
            "home"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                self.composer.selection.move_home(modifiers.shift);
                cx.stop_propagation();
                cx.notify();
            }
            "end"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function =>
            {
                self.composer
                    .selection
                    .move_end(&self.composer.text, modifiers.shift);
                cx.stop_propagation();
                cx.notify();
            }
            "a" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                self.composer.selection.select_all(&self.composer.text);
                cx.stop_propagation();
                cx.notify();
            }
            "c" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                if !self.composer.selection.range.is_empty() {
                    let text = self.composer.text
                        [self.composer.selection.range.clone()]
                        .to_string();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
                cx.stop_propagation();
            }
            "x" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                if !self.composer.selection.range.is_empty() {
                    let text = self.composer.text
                        [self.composer.selection.range.clone()]
                        .to_string();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    self.composer.selection.replace_text(&mut self.composer.text, None, "");
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "v" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                if let Some(text) = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text().map(|t| t.replace('\n', " ")))
                {
                    self.composer
                        .selection
                        .replace_text(&mut self.composer.text, None, &text);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn render_welcome(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        let session_id = self.session_id.to_string();
        let can_submit = self.composer.can_submit();
        let send_id = ElementId::Name(format!("composer-send-{session_id}").into());
        let send_button = div()
            .id(send_id)
            .w(px(34.0))
            .h(px(34.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(if can_submit { theme.primary } else { theme.border })
            .text_color(if can_submit { theme.card } else { theme.muted_foreground })
            .cursor(if can_submit { CursorStyle::PointingHand } else { CursorStyle::Arrow })
            .when(can_submit, |el| {
                el.on_click(cx.listener(|this, _event, _window, cx| {
                    this.send_current_message(cx);
                }))
            })
            .child(if self.composer.processing {
                spinner(14.0)
                    .text_color(theme.muted_foreground)
                    .with_animation(
                        "send-spinner",
                        Animation::new(std::time::Duration::from_millis(800)).repeat(),
                        |svg, delta| {
                            svg.with_transformation(Transformation::rotate(gpui::percentage(delta)))
                        },
                    )
                    .into_any_element()
            } else {
                arrow_up(14.0)
                    .text_color(if can_submit { theme.card } else { theme.muted_foreground })
                    .into_any_element()
            });

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .justify_center()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(18.0))
                    .max_w(px(720.0))
                    .px(px(24.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(54.0))
                            .h(px(54.0))
                            .rounded(px(16.0))
                            .bg(theme.primary)
                            .text_color(theme.card)
                            .font(UiTheme::display_font())
                            .font_weight(FontWeight::BOLD)
                            .text_2xl()
                            .shadow_sm()
                            .child("A"),
                    )
                    .child(
                        div()
                            .font(UiTheme::display_font())
                            .text_size(px(32.0))
                            .font_weight(FontWeight::BOLD)
                            .child("What are you building today?"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .text_center()
                            .child("Start with a prompt. Averroes can plan, edit, and run tools in your workspace."),
                    )
                    .child({
                        let active_name = self.active_workspace_name();
                        let root_str = self.workspace_root.display().to_string();
                        let selector_open = self.workspace_selector_open;
                        let selector = div()
                            .relative()
                            .child(
                                div()
                                    .id(ElementId::Name("chat-ws-selector".into()))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px(px(12.0))
                                    .py(px(7.0))
                                    .bg(theme.card)
                                    .border_1()
                                    .border_color(theme.border)
                                    .rounded_full()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .font(UiTheme::mono_font())
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.workspace_selector_open = !this.workspace_selector_open;
                                        cx.notify();
                                    }))
                                    .child("\u{1F4C1}")
                                    .child(format!("{} \u{2014} {}", active_name, root_str)),
                            );

                        if selector_open {
                            let dropdown = {
                                let workspace_id = self.workspace_id.clone();
                                let session_id = self.session_id.clone();
                                let mut items: Vec<gpui::AnyElement> = Vec::new();
                                for ws in &self.workspaces {
                                    let ws_id = ws.id.clone();
                                    let ws_name = ws.name.clone();
                                    let ws_root = ws.root.clone();
                                    let is_active = workspace_id.as_deref() == Some(&ws_id);
                                    let sid = session_id.clone();
                                    items.push(
                                        div()
                                            .id(ElementId::Name(format!("chat-ws-item-{}", ws_id).into()))
                                            .px(px(8.0))
                                            .py(px(4.0))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .justify_between()
                                            .text_sm()
                                            .text_color(theme.foreground)
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.accent))
                                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                                this.workspace_id = Some(ws_id.clone());
                                                this.workspace_root = ws_root.clone();
                                                this.workspace_selector_open = false;
                                                cx.emit(ChatViewEvent::WorkspaceChanged {
                                                    session_id: sid.clone(),
                                                    workspace_id: ws_id.clone(),
                                                });
                                                cx.notify();
                                            }))
                                            .child(ws_name.clone())
                                            .child(if is_active {
                                                div().child("\u{2713}").into_any_element()
                                            } else {
                                                div().into_any_element()
                                            })
                                            .into_any_element(),
                                    );
                                }
                                div()
                                    .absolute()
                                    .top(px(24.0))
                                    .left(px(0.0))
                                    .bg(theme.card)
                                    .border_1()
                                    .border_color(theme.border)
                                    .rounded(px(UiTheme::RADIUS))
                                    .shadow_sm()
                                    .p(px(4.0))
                                    .flex()
                                    .flex_col()
                                    .min_w(px(280.0))
                                    .children(items)
                            };
                            selector.child(dropdown)
                        } else {
                            selector
                        }
                    })
                    .child(
                        composer_surface(theme, true, false)
                            .w_full().max_w(px(720.0))
                            .cursor(CursorStyle::IBeam)
                            .track_focus(&self.focus_handle)
                            .on_key_down(cx.listener(Self::handle_key_down))
                            .px(px(14.0)).pt(px(14.0)).pb(px(8.0))
                            .min_h(px(100.0))
                            .child(div().flex_1().child(text_field_element(cx.entity(), self.focus_handle.clone())))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .pt(px(4.0))
                                    .child(self.render_model_selector(theme, cx))
                                    .child(self.render_reasoning_selector(theme, cx))
                                    .child(send_button),
                            ),
                    )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .font(UiTheme::mono_font())
                                    .child("⌘↵ to send  ·  shift+enter for a new line"),
                            ),
            )
    }

    fn send_current_message(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.composer.take_submission() else {
            return;
        };

        let Some(agent) = self.agent.as_ref().cloned() else {
            self.composer.text = text;
            return;
        };

        self.messages.push(MessageBubble::user(text.clone()));
        self.pending_reasoning.clear();
        self.save_messages();
        self.scroll_handle.scroll_to_bottom();
        self.composer.processing = true;
        cx.emit(ChatViewEvent::Submitted {
            session_id: self.session_id.clone(),
            text: text.clone(),
        });
        cx.notify();

        let factory = Arc::clone(&self.factory);
        let task = cx.spawn(async move |this, cx| {
            let mut stream = factory.spawn_agent_stream(agent, text);
            while let Some(event) = stream.next_event().await {
                match event {
                    AgentStreamEvent::ReasoningDelta { text } => {
                        _ = this.update(cx, |chat, cx| {
                            chat.pending_reasoning.push_str(&text);
                            chat.append_stream_text_with_reasoning("", Some(&text));
                            cx.notify();
                        });
                    }
                    AgentStreamEvent::TextDelta { text } => {
                        _ = this.update(cx, |chat, cx| {
                            chat.append_stream_text(&text);
                            cx.notify();
                        });
                    }
                }
            }

            let result = stream.finish().await;
            match result {
                Ok(Ok(response)) => {
                    _ = this.update(cx, |chat, cx| {
                        chat.complete_stream_response(response);
                        chat.composer.processing = false;
                        chat.agent_task = None;
                        cx.notify();
                    });
                }
                Ok(Err(error)) => {
                    _ = this.update(cx, |chat, cx| {
                        chat.messages
                            .push(MessageBubble::error(format!("Error: {error}")));
                        chat.save_messages();
                        chat.scroll_handle.scroll_to_bottom();
                        chat.composer.processing = false;
                        chat.agent_task = None;
                        cx.notify();
                    });
                }
                Err(error) => {
                    _ = this.update(cx, |chat, cx| {
                        chat.messages
                            .push(MessageBubble::error(format!("Error: {error}")));
                        chat.save_messages();
                        chat.scroll_handle.scroll_to_bottom();
                        chat.composer.processing = false;
                        chat.agent_task = None;
                        cx.notify();
                    });
                }
            }
        });
        self.agent_task = Some(task);
    }

    pub fn submit_composer(&mut self, cx: &mut Context<Self>) {
        self.send_current_message(cx);
    }

    fn toggle_reasoning_menu(&mut self, cx: &mut Context<Self>) {
        self.reasoning_menu_open = !self.reasoning_menu_open;
        self.attachment_menu_open = false;
        self.mode_menu_open = false;
        self.model_menu_open = false;
        self.effort_menu_open = false;
        cx.notify();
    }

    fn select_reasoning_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        self.reasoning_effort = effort.clone();
        self.reasoning_menu_open = false;
        if let Some(ref e) = effort {
            WorkspaceStore::save_last_reasoning_effort(e);
        }
        if let Some(ref agent) = self.agent {
            agent.set_reasoning_effort(effort);
        }
        cx.notify();
    }

    fn close_model_search(&mut self, cx: &mut Context<Self>) {
        self.model_menu_open = false;
        self.model_search.update(cx, |search, cx| {
            search.clear();
            cx.notify();
        });
        cx.notify();
    }

    fn toggle_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_menu_open = !self.model_menu_open;
        self.attachment_menu_open = false;
        self.mode_menu_open = false;
        self.effort_menu_open = false;
        self.reasoning_menu_open = false;
        if self.model_menu_open {
            self.model_search.update(cx, |search, cx| {
                search.clear();
                cx.notify();
            });
            let search_focus = self.model_search.read(cx).focus_handle();
            search_focus.focus(window, cx);
        } else {
            window.blur(cx);
            self.composer.focused = true;
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    fn select_model(&mut self, model_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let provider = Arc::clone(&self.factory.provider);
        let selected_model = model_id.clone();
        if let Some(agent) = self.agent.as_ref().cloned() {
            agent.reconfigure_provider(provider, selected_model, Arc::clone(&self.factory.governor));
        }
        self.selected_model = model_id.clone();
        WorkspaceStore::save_last_model(&model_id);

        if let Some(info) = self.model_catalog.iter().find(|m| m.id == model_id) {
            self.reasoning_effort = info.default_reasoning_effort.clone();
            if let Some(ref agent) = self.agent {
                agent.set_reasoning_effort(info.default_reasoning_effort.clone());
            }
        }

        self.model_menu_open = false;
        window.blur(cx);
        self.composer.focused = true;
        self.focus_handle.focus(window, cx);
        self.model_search.update(cx, |search, cx| {
            search.clear();
            cx.notify();
        });
        cx.emit(ChatViewEvent::ModelChanged(model_id));
        cx.notify();
    }

    fn toggle_attachment_menu(&mut self, cx: &mut Context<Self>) {
        self.attachment_menu_open = !self.attachment_menu_open;
        self.mode_menu_open = false;
        self.model_menu_open = false;
        self.effort_menu_open = false;
        self.reasoning_menu_open = false;
        cx.notify();
    }

    fn toggle_mode_menu(&mut self, cx: &mut Context<Self>) {
        self.mode_menu_open = !self.mode_menu_open;
        self.attachment_menu_open = false;
        self.model_menu_open = false;
        self.effort_menu_open = false;
        self.reasoning_menu_open = false;
        cx.notify();
    }

    fn toggle_effort_menu(&mut self, cx: &mut Context<Self>) {
        self.effort_menu_open = !self.effort_menu_open;
        self.attachment_menu_open = false;
        self.mode_menu_open = false;
        self.model_menu_open = false;
        self.reasoning_menu_open = false;
        cx.notify();
    }

    fn select_mode(&mut self, mode: ComposerMode, cx: &mut Context<Self>) {
        self.composer.mode = mode;
        self.mode_menu_open = false;
        cx.notify();
    }

    fn select_effort(&mut self, effort: EffortLevel, cx: &mut Context<Self>) {
        self.composer.effort = effort;
        self.effort_menu_open = false;
        cx.notify();
    }

    fn insert_context(&mut self, prefix: &str, cx: &mut Context<Self>) {
        self.composer
            .selection
            .replace_text(&mut self.composer.text, None, prefix);
        self.attachment_menu_open = false;
        self.composer.focused = true;
        cx.notify();
    }

    fn dropdown_trigger(
        theme: UiTheme,
        label: impl Into<SharedString>,
        open: bool,
        filled: bool,
    ) -> Div {
        let text: SharedString = format!("{} \u{25BE}", label.into()).into();
        div()
            .px(px(9.0))
            .py(px(5.5))
            .rounded(px(UiTheme::RADIUS))
            .text_xs()
            .font_weight(FontWeight::NORMAL)
            .text_color(theme.foreground)
            .bg(if open || filled { theme.accent } else { rgba(0x00000000) })
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .child(text)
    }

    fn model_trigger(
        theme: UiTheme,
        provider: &str,
        display_name: &str,
        open: bool,
    ) -> Div {
        div()
            .px(px(8.0))
            .py(px(5.5))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .rounded(px(UiTheme::RADIUS))
            .text_xs()
            .font_weight(FontWeight::NORMAL)
            .text_color(theme.muted_foreground)
            .bg(if open { theme.accent } else { rgba(0x00000000) })
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .child(provider_logo(provider, 16.0))
            .child(div().flex_1().min_w(px(0.0)).truncate().child(display_name.to_string()))
            .child(chevron_down_icon(10.0))
    }

    fn dropdown_item(theme: UiTheme, label: impl Into<SharedString>, selected: bool) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .w_full().max_w(px(768.0))
            .min_h(px(28.0))
            .px(px(9.0))
            .py(px(4.0))
            .rounded(px(5.0))
            .text_xs()
            .text_color(if selected { theme.primary } else { theme.foreground })
            .when(selected, |element| element.font_weight(FontWeight::MEDIUM))
            .hover(|style| style.bg(theme.accent))
            .child(div().flex_1().child(label.into()))
            .child(if selected {
                div().text_color(theme.primary).font_weight(FontWeight::MEDIUM).child("✓").into_any_element()
            } else {
                div().into_any_element()
            })
    }

    fn dropdown_panel(theme: UiTheme, width: f32, items: Vec<AnyElement>) -> impl IntoElement {
        deferred(
            div()
                .absolute()
                .bottom(px(34.0))
                .left(px(0.0))
                .w(px(width))
                .bg(theme.card)
                .border_1()
                .border_color(theme.border)
                .rounded(px(UiTheme::RADIUS))
                .shadow_md()
                .p(px(4.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(items),
        )
        .with_priority(100)
    }

    fn model_row(theme: UiTheme, model: &ModelInfo, selected: bool) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .w_full().max_w(px(768.0))
            .min_h(px(30.0))
            .px(px(9.0))
            .py(px(4.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .child(provider_logo(&model.provider, 14.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_xs()
                    .truncate()
                    .text_color(if selected { theme.primary } else { theme.foreground })
                    .when(selected, |element| element.font_weight(FontWeight::MEDIUM))
                    .child(model.display_name.clone()),
            )
            .child(if selected {
                div().text_color(theme.primary).font_weight(FontWeight::BOLD).child("✓").into_any_element()
            } else {
                div().into_any_element()
            })
    }

    fn render_reasoning_selector(&mut self, theme: UiTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.reasoning_effort.as_deref().unwrap_or("auto").to_string();
        let is_open = self.reasoning_menu_open;
        let current_effort = self.reasoning_effort.clone();
        let is_none = current_effort.is_none();
        div().relative().child(
            Self::dropdown_trigger(theme, label.clone(), is_open, false)
                .id(self.element_id("composer-reasoning"))
                .text_color(theme.muted_foreground)
                .on_click(cx.listener(|this, _event, _window, cx| {
                    cx.stop_propagation();
                    this.toggle_reasoning_menu(cx);
                })),
        )
        .when(is_open, move |el| {
            let mut items = vec![
                Self::dropdown_item(theme, "auto", is_none)
                    .id(self.element_id("composer-reasoning-auto"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        cx.stop_propagation();
                        this.select_reasoning_effort(None, cx);
                    }))
                    .into_any_element(),
            ];
            for &effort in &["none", "low", "medium", "high", "xhigh", "max"] {
                let e = effort.to_string();
                let selected = current_effort.as_deref() == Some(effort);
                items.push(
                    Self::dropdown_item(theme, e.clone(), selected)
                        .id(self.element_id(&format!("composer-reasoning-{}", e)))
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.select_reasoning_effort(Some(e.clone()), cx);
                        }))
                        .into_any_element(),
                );
            }
            el.child(Self::dropdown_panel(theme, 138.0, items))
        })
    }

    fn active_workspace_name(&self) -> String {
        self.workspace_id
            .as_ref()
            .and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
            .map(|w| w.name.clone())
            .unwrap_or_else(|| "workspace".into())
    }

    fn render_model_selector(&mut self, theme: UiTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let provider = self.factory.provider.provider_name();
        let selected = self.model_catalog.iter().find(|m| m.id == self.selected_model);
        let display_name = selected.map(|m| m.display_name.clone()).unwrap_or_else(|| self.selected_model.clone());
        let provider = selected.map(|m| m.provider.as_str()).unwrap_or(provider);

        div().relative().child(
            Self::model_trigger(theme, provider, &display_name, self.model_menu_open)
                .id(self.element_id("composer-model"))
                .max_w(px(190.0))
                .on_click(cx.listener(|this, _event, window, cx| {
                    cx.stop_propagation();
                    this.toggle_model_menu(window, cx);
                })),
        )
        .when(self.model_menu_open, |el| el.child(self.render_model_panel(theme, cx)))
    }

    fn render_model_panel(&mut self, theme: UiTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.model_search.clone();
        let search_focus = self.model_search.read(cx).focus_handle();
        let query = self.model_search.read(cx).value().to_string();
        let matches = filter_models(&self.model_catalog, &query);
        let provider = self.factory.provider.provider_name();
        let selected_model = self.selected_model.clone();
        let no_matches = matches.is_empty();
        let mut rows: Vec<AnyElement> = Vec::new();

        if !matches.is_empty() {
            rows.push(
                div().px(px(9.0)).pt(px(5.0)).pb(px(3.0)).text_xs()
                    .font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground)
                    .child(provider_label(provider)).into_any_element(),
            );
        }

        for model in matches {
            let model_id = model.id.clone();
            let row = Self::model_row(theme, model, model.id == selected_model)
                .id(self.element_id(&format!("composer-model-{}", model.id)))
                .on_click(cx.listener(move |this, _event, window, cx| {
                    cx.stop_propagation();
                    this.select_model(model_id.clone(), window, cx);
                }));
            rows.push(row.into_any_element());
        }

        if no_matches && !self.model_catalog.is_empty() {
            rows.push(div().px(px(9.0)).py(px(10.0)).text_sm().text_color(theme.muted_foreground)
                .child("No se encontraron modelos").into_any_element());
        }
        if self.model_catalog.is_empty() {
            rows.push(div().px(px(9.0)).py(px(10.0)).text_sm().text_color(theme.muted_foreground)
                .child("No hay modelos disponibles").into_any_element());
        }

        let search_row = div()
            .id(self.element_id("composer-model-search"))
            .track_focus(&search_focus)
            .flex().flex_row().items_center().gap(px(6.0))
            .h(px(30.0)).px(px(8.0)).rounded(px(5.0))
            .bg(theme.card).border_1().border_color(theme.border)
            .on_click(cx.listener(|this, _event, window, cx| {
                this.model_search.read(cx).focus_handle().focus(window, cx);
                cx.stop_propagation();
            }))
            .child(div().text_color(theme.muted_foreground).child("\u{2315}"))
            .child(div().flex_1().min_w(px(0.0)).child(text_field_element(search, search_focus)))
            .child(if query.is_empty() { div().into_any_element() } else {
                div().id(self.element_id("composer-model-clear")).text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        cx.stop_propagation();
                        this.model_search.update(cx, |s, cx| { s.clear(); cx.notify(); });
                        cx.notify();
                    }))
                    .child("\u{00D7}").into_any_element()
            });

        deferred(
            div().absolute().bottom(px(34.0)).left(px(0.0)).w(px(290.0))
                .bg(theme.card).border_1().border_color(theme.border)
                .rounded(px(UiTheme::RADIUS)).shadow_md().p(px(4.0))
                .flex().flex_col().gap(px(4.0))
                .child(search_row)
                .child(
                    div().id(self.element_id("composer-model-list"))
                        .max_h(px(300.0)).overflow_y_scroll()
                        .flex().flex_col().gap(px(2.0))
                        .children(rows),
                ),
        ).with_priority(100)
    }


}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TextField for ChatView {
    fn text(&self) -> &str {
        &self.composer.text
    }

    fn placeholder(&self) -> &str {
        "Ask anything, / for commands, @ for context\u{2026}"
    }

    fn selection_range(&self) -> Range<usize> {
        self.composer.selection.range.clone()
    }

    fn selection_reversed(&self) -> bool {
        self.composer.selection.reversed
    }

    fn marked_range(&self) -> Option<Range<usize>> {
        self.composer.selection.marked_range.clone()
    }

    fn cursor_offset(&self) -> usize {
        self.composer.selection.cursor_offset()
    }

    fn set_last_layout(&mut self, lines: Vec<ShapedLine>, line_height: Pixels, bounds: Bounds<Pixels>) {
        self.last_layout = Some(lines);
        self.last_line_height = Some(line_height);
        self.last_bounds = Some(bounds);
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_on_render {
            self.focus_on_render = false;
            self.composer.focused = true;
            self.focus_handle.focus(window, cx);
        }

        self.update_last_read_only(cx);

        let window_size = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => bounds.size,
            _ => size(px(1200.0), px(800.0)),
        };
        let viewport_h = window_size.height - px(240.0);

        let theme = self.theme;
        let session_id = self.session_id.to_string();
        let is_empty = self.messages.is_empty() && !self.composer.processing;

        if is_empty {
            return self.render_welcome(window, cx).into_any_element();
        }

        let can_submit = self.composer.can_submit();
        let send_id = ElementId::Name(format!("composer-send-{session_id}").into());
        let send_button = div()
            .id(send_id)
            .w(px(34.0))
            .h(px(34.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(if can_submit { theme.primary } else { theme.border })
            .text_color(if can_submit { theme.card } else { theme.muted_foreground })
            .cursor(if can_submit { CursorStyle::PointingHand } else { CursorStyle::Arrow })
            .when(can_submit, |el| {
                el.on_click(cx.listener(|this, _event, _window, cx| {
                    this.send_current_message(cx);
                }))
            })
            .child(if self.composer.processing {
                spinner(14.0)
                    .text_color(theme.muted_foreground)
                    .with_animation(
                        "send-spinner",
                        Animation::new(std::time::Duration::from_millis(800)).repeat(),
                        |svg, delta| {
                            svg.with_transformation(Transformation::rotate(gpui::percentage(delta)))
                        },
                    )
                    .into_any_element()
            } else {
                arrow_up(14.0)
                    .text_color(if can_submit { theme.card } else { theme.muted_foreground })
                    .into_any_element()
            });

        let ctx_menu = if self.context_menu_open {
            let pos = self.context_menu_pos;
            let id_copy = self.element_id("ctx-copy");
            let id_cut = self.element_id("ctx-cut");
            let id_paste = self.element_id("ctx-paste");
            let id_all = self.element_id("ctx-select-all");
            Some(
                div()
                    .absolute()
                    .left(pos.x)
                    .top(pos.y)
                    .bg(theme.card)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(UiTheme::RADIUS))
                    .p(px(4.0))
                    .flex()
                    .flex_col()
                    .child(
                        button(theme, ButtonVariant::Ghost, "Copy")
                            .id(id_copy)
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if !this.composer.selection.range.is_empty() {
                                    let text = this.composer.text
                                        [this.composer.selection.range.clone()]
                                        .to_string();
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                }
                                this.context_menu_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        button(theme, ButtonVariant::Ghost, "Cut")
                            .id(id_cut)
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if !this.composer.selection.range.is_empty() {
                                    let text = this.composer.text
                                        [this.composer.selection.range.clone()]
                                        .to_string();
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                    this.composer.selection.replace_text(&mut this.composer.text, None, "");
                                }
                                this.context_menu_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        button(theme, ButtonVariant::Ghost, "Paste")
                            .id(id_paste)
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if let Some(text) = cx
                                    .read_from_clipboard()
                                    .and_then(|item| item.text().map(|t| t.replace('\n', " ")))
                                {
                                    this.composer.selection.replace_text(&mut this.composer.text, None, &text);
                                }
                                this.context_menu_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        button(theme, ButtonVariant::Ghost, "Select All")
                            .id(id_all)
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.composer.selection.select_all(&this.composer.text);
                                this.context_menu_open = false;
                                cx.notify();
                            })),
                    ),
            )
        } else {
            None
        };

        let mut composer_input = div()
            .relative()
            .child(
                composer_surface(theme, self.composer.focused, self.composer.processing)
                    .id(self.element_id("composer-input"))
                    .track_focus(&self.focus_handle)
                    .cursor(CursorStyle::IBeam)
                    .on_click(cx.listener(Self::on_composer_click))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_composer_mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::on_composer_context_menu))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_composer_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_composer_mouse_up))
                    .on_mouse_move(cx.listener(Self::on_composer_mouse_move))
                    .on_key_down(cx.listener(Self::handle_key_down))
                    .px(px(14.0)).pt(px(14.0)).pb(px(8.0))
                    .min_h(px(116.0))
                    .flex_col()
                    .child(
                        div().flex_1().child(text_field_element(cx.entity(), self.focus_handle.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .pt(px(4.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .id(self.element_id("composer-add"))
                                            .px(px(9.0))
                                            .py(px(5.5))
                                            .rounded(px(UiTheme::RADIUS))
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.toggle_attachment_menu(cx);
                                            }))
                                            .child(plus_icon(14.0)),
                                    )
                                    .child({
                                        let mut container = div().relative().child(
                                            Self::dropdown_trigger(theme, self.composer.mode.label(), self.mode_menu_open, false)
                                                .id(self.element_id("composer-mode"))
                                                .text_color(theme.muted_foreground)
                                                .on_click(cx.listener(|this, _event, _window, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_mode_menu(cx);
                                                })),
                                        );
                                        if self.mode_menu_open {
                                            container = container.child(Self::dropdown_panel(theme, 158.0, vec![
                                                Self::dropdown_item(theme, ComposerMode::Build.label(), matches!(self.composer.mode, ComposerMode::Build))
                                                    .id(self.element_id("composer-mode-build"))
                                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                                        cx.stop_propagation();
                                                        this.select_mode(ComposerMode::Build, cx);
                                                    }))
                                                    .into_any_element(),
                                                Self::dropdown_item(theme, ComposerMode::Plan.label(), matches!(self.composer.mode, ComposerMode::Plan))
                                                    .id(self.element_id("composer-mode-plan"))
                                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                                        cx.stop_propagation();
                                                        this.select_mode(ComposerMode::Plan, cx);
                                                    }))
                                                    .into_any_element(),
                                            ]));
                                        }
                                        container
                                    })
                                    .child(self.render_model_selector(theme, cx))
                                    .child(self.render_reasoning_selector(theme, cx)),
                            )
                            .child(send_button),
                    ),
            );
        if let Some(menu) = ctx_menu {
            composer_input = composer_input.child(menu);
        }

        let last_assistant_idx = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == message::MessageRole::Assistant)
            .map(|(i, _)| i);

        div()
            .id(ElementId::Name(format!("chat-{}", self.session_id).into()))
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .bg(theme.background)
            .on_click(cx.listener(|this, event: &ClickEvent, _window, cx| {
                if event.standard_click() {
                    this.context_menu_open = false;
                    this.model_menu_open = false;
                    this.reasoning_menu_open = false;
                    this.message_context_menu_open = false;
                    cx.notify();
                }
            }))
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .child(
                div()
                    .flex_none()
                    .h(px(48.0))
                    .px(px(28.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .bg(theme.card)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .w(px(7.0))
                                    .h(px(7.0))
                                    .rounded_full()
                                    .bg(theme.brand_orange),
                            )
                            .child(
                                div()
                                    .font(UiTheme::display_font())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child("Conversation"),
                            )
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("in {}", self.active_workspace_name())),
                            ),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .font(UiTheme::mono_font())
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(self.workspace_root.display().to_string()),
                    ),
            )
            .child({
                let scroll_offset = self.scroll_handle.offset();
                let max_offset = self.scroll_handle.max_offset();
                let scrollable = max_offset.y > px(1.0);
                let track_h = viewport_h;
                let thumb_h = if scrollable {
                    (track_h / (track_h + max_offset.y) * track_h).max(px(28.0))
                } else {
                    px(0.0)
                };
                let thumb_top = if scrollable && max_offset.y > px(0.0) {
                    (-scroll_offset.y) / max_offset.y * (track_h - thumb_h)
                } else {
                    px(0.0)
                };

                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
                    .group("chat-scroll")
                    .hover(|s| s)
                    .child(
                        div()
                            .id(ElementId::Name("chat-messages".into()))
                            .flex()
                            .flex_col()
                            .flex_1()
                            .w_full()
                            .min_w(px(0.0))
                            .min_h(px(0.0))
                            .overflow_y_scroll()
                            .overflow_x_hidden()
                            .items_center()
                            .track_scroll(&self.scroll_handle)
                            .child(
                                div()
                                    .w_full().max_w(px(800.0))
                                    .p(px(28.0))
                                    .gap(px(14.0))
                                    .children(self.messages.iter().enumerate().map(|(idx, msg)| {
                        let is_user = msg.role == message::MessageRole::User;
                        let is_assistant = msg.role == message::MessageRole::Assistant;
                        let _is_last_assistant = last_assistant_idx == Some(idx);
                        let (background, foreground) = match msg.role {
                            message::MessageRole::User => {
                                (Some(theme.accent), theme.foreground)
                            }
                            _ => (None, theme.foreground),
                        };
                        let content = msg.content.clone();
                        let reasoning = msg.reasoning.clone();
                        let mut bubble = div();
                        if is_user {
                            bubble = bubble
                                .bg(background.unwrap())
                                .border_1()
                                .border_color(theme.accent)
                                .rounded(px(10.0))
                                .p(px(14.0))
                                .max_w(px(480.0))
                                .text_sm()
                                .text_color(foreground);
                            bubble = bubble
                                .child(
                                    div()
                                        .text_xs()
                                        .font(UiTheme::mono_font())
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme.primary)
                                        .mb(px(6.0))
                                        .child("YOU"),
                                )
                                .child(
                                    TextView::markdown(("user-message", idx), content.clone())
                                        .selectable(true)
                                        .w_full()
                                        .min_w(px(0.0))
                                        .text_sm()
                                        .text_color(foreground),
                                );
                        }
                        if msg.role == message::MessageRole::Error {
                            bubble = bubble
                                .text_sm()
                                .text_color(theme.destructive)
                                .child(format!("Error: {}", content));
                        }
                        if is_assistant {
                            bubble = bubble
                                .w_full().max_w(px(768.0))
                                .min_w(px(0.0));
                            bubble = bubble
                                .bg(theme.card)
                                .border_1()
                                .border_color(theme.border)
                                .rounded(px(10.0))
                                .p(px(16.0))
                                .shadow_sm()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(8.0))
                                        .mb(px(10.0))
                                        .child(
                                            div()
                                                .w(px(22.0))
                                                .h(px(22.0))
                                                .rounded(px(7.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .bg(theme.primary)
                                                .text_color(theme.card)
                                                .font(UiTheme::display_font())
                                                .font_weight(FontWeight::BOLD)
                                                .text_xs()
                                                .child("A"),
                                        )
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_sm()
                                                .child("Averroes"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .font(UiTheme::mono_font())
                                                .child("assistant"),
                                        ),
                                );
                            if let Some(ref r) = reasoning {
                                bubble = bubble.child(
                                    div()
                                        .border_l_2()
                                        .border_color(theme.muted_foreground)
                                        .pl(px(8.0))
                                        .ml(px(4.0))
                                        .mb(px(6.0))
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(
                                            TextView::markdown(("reasoning", idx), r.clone())
                                                .selectable(true)
                                                .w_full()
                                                .min_w(px(0.0))
                                                .text_xs()
                                                .text_color(theme.muted_foreground),
                                        ),
                                );
                            }
                            bubble = bubble.child(
                                TextView::markdown(("assistant-message", idx), content.clone())
                                    .selectable(true)
                                    .w_full()
                                    .min_w(px(0.0))
                                    .text_sm()
                                    .text_color(foreground),
                            );
                        }
                        div()
                            .w_full().max_w(px(768.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .when(is_user, |el| el.items_end())
                            .child(bubble)
                    }))
                    .child(if self.composer.processing {
                        div()
                            .text_xs()
                            .text_color(theme.primary)
                            .font_weight(FontWeight::BOLD)
                            .font(UiTheme::mono_font())
                            .child("thinking...")
                    } else {
                        div()
                    }),
                    )
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(4.0))
                            .top(px(4.0))
                            .w(px(4.0))
                            .h_full()
                            .rounded(px(2.0))
                            .opacity(0.0)
                            .group_hover("chat-scroll", |style| style.opacity(1.0))
                            .when(!scrollable, |el| el.hidden())
                            .child(
                                div()
                                    .absolute()
                                    .left(px(0.0))
                                    .top(thumb_top)
                                    .w(px(4.0))
                                    .h(thumb_h)
                                    .rounded(px(2.0))
                                    .bg(theme.muted_foreground)
                                    .opacity(0.3),
                            ),
                    )
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full().max_w(px(768.0))
                            .min_w(px(0.0))
                            .flex_none()
                            .p(px(16.0))
                            .child(composer_input),
                    ),
            ).into_any_element()
    }
}

fn context_menu_local_position(
    window_position: Point<Pixels>,
    text_bounds: Option<Bounds<Pixels>>,
) -> Point<Pixels> {
    text_bounds
        .map(|bounds| {
            point(
                window_position.x - bounds.left() + px(14.0),
                window_position.y - bounds.top() + px(14.0),
            )
        })
        .unwrap_or(point(px(16.0), px(16.0)))
}

fn context_menu_position(
    window_position: Point<Pixels>,
    text_bounds: Option<Bounds<Pixels>>,
    window: &Window,
) -> Point<Pixels> {
    const MENU_WIDTH: Pixels = px(140.0);
    const MENU_HEIGHT: Pixels = px(170.0);

    let window_size = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => bounds.size,
        _ => size(px(1200.0), px(800.0)),
    };

    let Some(bounds) = text_bounds else {
        let fallback = context_menu_local_position(window_position, None);
        if fallback.x + MENU_WIDTH > window_size.width {
            return point(px(0.0), fallback.y);
        }
        if fallback.y + MENU_HEIGHT > window_size.height {
            return point(fallback.x, px(0.0));
        }
        return fallback;
    };

    let composer_origin_x = bounds.left() - px(14.0);
    let composer_origin_y = bounds.top() - px(14.0);

    let mut local_x = window_position.x - bounds.left() + px(14.0);
    let mut local_y = window_position.y - bounds.top() + px(14.0);

    let window_menu_x = composer_origin_x + local_x;
    let window_menu_y = composer_origin_y + local_y;

    if window_menu_x + MENU_WIDTH > window_size.width {
        local_x = (local_x - MENU_WIDTH).max(px(0.0));
    }

    if window_menu_y + MENU_HEIGHT > window_size.height {
        local_y = (local_y - MENU_HEIGHT).max(px(0.0));
    }

    point(local_x.max(px(0.0)), local_y.max(px(0.0)))
}

#[cfg(test)]
mod tests {
    use super::context_menu_local_position;
    use gpui::{point, px, size, Bounds};

    #[test]
    fn context_menu_position_is_relative_to_composer() {
        let window_position = point(px(430.0), px(720.0));
        let text_bounds = Bounds::new(point(px(300.0), px(700.0)), size(px(400.0), px(24.0)));

        assert_eq!(
            context_menu_local_position(window_position, Some(text_bounds)),
            point(px(144.0), px(34.0))
        );
    }

    #[test]
    fn context_menu_without_text_layout_uses_visible_local_fallback() {
        assert_eq!(
            context_menu_local_position(point(px(900.0), px(700.0)), None),
            point(px(16.0), px(16.0))
        );
    }
}
