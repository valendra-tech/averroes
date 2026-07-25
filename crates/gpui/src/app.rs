use gpui::*;

use crate::runtime::AgentFactory;
use crate::theme::Theme;
use crate::views::{chat::ChatView, settings::SettingsView, sidebar::Sidebar};
use averroes_core::agent::Agent;
use std::sync::Arc;

pub struct AverroesApp {
    sidebar: Entity<Sidebar>,
    chat: Entity<ChatView>,
    settings: Entity<SettingsView>,
    active_view: ActiveView,
    theme: Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    Chat,
    Settings,
}

impl AverroesApp {
    pub fn new(
        cx: &mut Context<Self>,
        agent: Option<Arc<Agent>>,
        factory: Arc<AgentFactory>,
    ) -> Self {
        let theme = Theme::default();

        let sidebar = cx.new(|cx| Sidebar::new(cx));
        let chat = cx.new(|cx| ChatView::new(cx, agent, factory));
        let settings = cx.new(|cx| SettingsView::new(cx));

        Self {
            sidebar,
            chat,
            settings,
            active_view: ActiveView::Chat,
            theme,
        }
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, _cx| {
            sidebar.toggle();
        });
        cx.notify();
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme = if matches!(self.theme, _) {
            Theme::light()
        } else {
            Theme::dark()
        };
        self.sidebar.update(cx, |sidebar, _cx| {
            sidebar.set_theme(self.theme);
        });
        self.chat.update(cx, |chat, _cx| {
            chat.set_theme(self.theme);
        });
        self.settings.update(cx, |settings, _cx| {
            settings.set_theme(self.theme);
        });
        cx.notify();
    }

    fn switch_to_chat(&mut self, cx: &mut Context<Self>) {
        self.active_view = ActiveView::Chat;
        cx.notify();
    }

    fn switch_to_settings(&mut self, cx: &mut Context<Self>) {
        self.active_view = ActiveView::Settings;
        cx.notify();
    }
}

impl Render for AverroesApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .flex_row()
                    .flex_1()
                    .child(self.sidebar.clone())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(match self.active_view {
                                ActiveView::Chat => self.chat.clone().into_any_element(),
                                ActiveView::Settings => {
                                    self.settings.clone().into_any_element()
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .px_4()
                    .py_1()
                    .gap_4()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if matches!(self.active_view, ActiveView::Chat) {
                                theme.accent
                            } else {
                                theme.muted
                            })
                            .cursor_pointer()
                            .hover(|style| {
                                if !matches!(self.active_view, ActiveView::Chat) {
                                    style.bg(theme.border)
                                } else {
                                    style
                                }
                            })
                            .id(ElementId::Name("tab-chat".into()))
                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.switch_to_chat(cx);
                            }))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .child("Chat"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(
                                if matches!(self.active_view, ActiveView::Settings) {
                                    theme.accent
                                } else {
                                    theme.muted
                                },
                            )
                            .cursor_pointer()
                            .hover(|style| {
                                if !matches!(self.active_view, ActiveView::Settings) {
                                    style.bg(theme.border)
                                } else {
                                    style
                                }
                            })
                            .id(ElementId::Name("tab-settings".into()))
                            .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                this.switch_to_settings(cx);
                            }))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .child("Settings"),
                    ),
            )
    }
}
