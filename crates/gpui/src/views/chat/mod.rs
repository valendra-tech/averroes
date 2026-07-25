pub mod message;

use gpui::*;

use crate::theme::Theme;
use message::MessageBubble;
use averroes_core::agent::Agent;
use std::sync::Arc;

pub struct ChatView {
    messages: Vec<MessageBubble>,
    input_text: String,
    theme: Theme,
    thinking: bool,
    agent: Option<Arc<Agent>>,
}

impl ChatView {
    pub fn new(_cx: &mut Context<Self>, agent: Option<Arc<Agent>>) -> Self {
        let mut messages = Vec::new();
        if agent.is_some() {
            messages.push(MessageBubble::assistant("Averroes AI ready."));
        } else {
            messages.push(MessageBubble::assistant("No AI provider configured."));
        }

        Self {
            messages,
            input_text: String::new(),
            theme: Theme::default(),
            thinking: false,
            agent,
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    fn send_message(&mut self, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.input_text);
        if text.is_empty() {
            return;
        }

        if let Some(ref agent) = self.agent {
            self.messages.push(MessageBubble::user(text.clone()));
            cx.notify();

            let agent = Arc::clone(agent);
            self.thinking = true;

            cx.spawn(async move |this, cx| {
                let result = agent.run(&text).await;
                match result {
                    Ok(response) => {
                        _ = this.update(cx, |chat, cx| {
                            chat.messages.push(MessageBubble::assistant(response));
                            chat.thinking = false;
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        _ = this.update(cx, |chat, cx| {
                            chat.messages.push(MessageBubble::assistant(format!("Error: {}", e)));
                            chat.thinking = false;
                            cx.notify();
                        });
                    }
                }
            }).detach();
        }
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .p_4()
                    .gap_2()
                    .children(self.messages.iter().map(|msg| {
                        let bg = match msg.role {
                            message::MessageRole::User => theme.accent,
                            message::MessageRole::Assistant => theme.surface,
                            message::MessageRole::System => theme.surface,
                        };
                        let color = match msg.role {
                            message::MessageRole::User => theme.bg,
                            message::MessageRole::Assistant => theme.fg,
                            message::MessageRole::System => theme.muted,
                        };

                        div()
                            .flex()
                            .flex_row()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .w_full()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted)
                                            .child(match msg.role {
                                                message::MessageRole::User => "You",
                                                message::MessageRole::Assistant => "Averroes",
                                                message::MessageRole::System => "System",
                                            }),
                                    )
                                    .child(
                                        div()
                                            .p_2()
                                            .rounded_md()
                                            .bg(bg)
                                            .text_color(color)
                                            .text_sm()
                                            .child(msg.content.clone()),
                                    ),
                            )
                    }))
                    .child(if self.thinking {
                        div()
                            .text_xs()
                            .text_color(theme.muted)
                            .child("thinking...")
                    } else {
                        div()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .border_t_1()
                    .border_color(theme.border)
                    .p_3()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .bg(theme.surface)
                            .rounded_md()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(theme.fg)
                            .child(if self.input_text.is_empty() {
                                String::from("Type a message...")
                            } else {
                                self.input_text.clone()
                            }),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .bg(theme.accent)
                            .text_color(theme.bg)
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .child("Send"),
                    ),
            )
    }
}
