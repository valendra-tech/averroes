pub mod message;

use gpui::*;

use crate::theme::Theme;
use message::MessageBubble;

pub struct ChatView {
    messages: Vec<MessageBubble>,
    input: Entity<Input>,
    theme: Theme,
}

impl ChatView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            messages: vec![MessageBubble::assistant("Averroes AI ready.")],
            input: cx.new(|_cx| Input::new()),
            theme: Theme::default(),
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
}

struct Input {
    text: String,
}

impl Input {
    fn new() -> Self {
        Self {
            text: String::new(),
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
                    })),
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
                            .child("Type a message..."),
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
