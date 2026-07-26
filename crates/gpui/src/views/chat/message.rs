use averroes_core::provider::types::{ChatMessage, MessageContent, Role as CoreRole};

pub struct MessageBubble {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Error,
}

impl From<ChatMessage> for MessageBubble {
    fn from(msg: ChatMessage) -> Self {
        let role = match msg.role {
            CoreRole::User => MessageRole::User,
            CoreRole::Assistant => MessageRole::Assistant,
            CoreRole::System => MessageRole::System,
            CoreRole::Tool => MessageRole::System,
        };
        let content = match msg.content {
            MessageContent::Text(text) => text,
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    averroes_core::provider::types::ContentPart::Text { text } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        Self { role, content }
    }
}

impl MessageBubble {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Error,
            content: content.into(),
        }
    }
}
