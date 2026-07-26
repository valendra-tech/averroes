pub mod strategies;

use crate::provider::types::ChatMessage;
use crate::provider::Provider;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct CompactionConfig {
    pub strategy: CompactionStrategyType,
    pub threshold: f64,
    pub keep_last: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            strategy: CompactionStrategyType::Hybrid,
            threshold: 0.8,
            keep_last: 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStrategyType {
    Trim,
    Summary,
    Hybrid,
}

pub struct CompactedContext {
    pub messages: Vec<ChatMessage>,
    pub original_count: usize,
    pub compacted_count: usize,
}

#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    fn should_compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
    ) -> bool;

    async fn compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
        provider: Option<&dyn Provider>,
        model: &str,
    ) -> Result<CompactedContext>;

    fn estimate_tokens(&self, messages: &[ChatMessage]) -> usize;
}

pub type Result<T> = std::result::Result<T, CompactionError>;

pub fn sanitize_tool_history(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut sanitized = Vec::new();
    let mut pending_tool_turn: Option<(ChatMessage, Vec<String>, Vec<ChatMessage>)> = None;

    for message in messages {
        if let Some((assistant, pending_ids, tool_messages)) = pending_tool_turn.as_mut() {
            if message.role == crate::provider::types::Role::Tool {
                let Some(tool_call_id) = message.tool_call_id.as_deref() else {
                    continue;
                };
                if let Some(index) = pending_ids.iter().position(|id| id == tool_call_id) {
                    pending_ids.remove(index);
                    tool_messages.push(message);
                    if pending_ids.is_empty() {
                        sanitized.push(assistant.clone());
                        sanitized.append(tool_messages);
                        pending_tool_turn = None;
                    }
                    continue;
                }
                continue;
            }

            pending_tool_turn = None;
        }

        if message.role == crate::provider::types::Role::Assistant {
            let pending_ids = message
                .tool_calls
                .as_ref()
                .filter(|calls| !calls.is_empty())
                .map(|tool_calls| tool_calls.iter().map(|call| call.id.clone()).collect());
            if let Some(pending_ids) = pending_ids {
                pending_tool_turn = Some((message, pending_ids, Vec::new()));
                continue;
            }
        }

        if message.role != crate::provider::types::Role::Tool {
            sanitized.push(message);
        }
    }

    sanitized
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("Summary generation failed: {0}")]
    SummaryFailed(String),
    #[error("{0}")]
    Other(String),
}

impl From<crate::provider::ProviderError> for CompactionError {
    fn from(e: crate::provider::ProviderError) -> Self {
        CompactionError::SummaryFailed(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FunctionCall, MessageContent, Role, ToolCall};

    fn message(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: MessageContent::Text(text.into()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn sanitize_tool_history_drops_orphans_and_incomplete_turns() {
        let tool_call = ToolCall {
            id: "call-1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "file_read".into(),
                arguments: "{}".into(),
            },
        };
        let assistant = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text("tool".into()),
            tool_call_id: None,
            tool_calls: Some(vec![tool_call.clone()]),
        };
        let tool = ChatMessage {
            role: Role::Tool,
            content: MessageContent::Text("result".into()),
            tool_call_id: Some(tool_call.id.clone()),
            tool_calls: None,
        };
        let incomplete = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text("unfinished".into()),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-2".into(),
                ..tool_call
            }]),
        };

        let sanitized = sanitize_tool_history(vec![
            message(Role::Tool, "orphan"),
            assistant,
            tool,
            message(Role::User, "next"),
            incomplete,
        ]);

        assert_eq!(sanitized.len(), 3);
        assert_eq!(sanitized[0].role, Role::Assistant);
        assert_eq!(sanitized[1].role, Role::Tool);
        assert_eq!(sanitized[2].role, Role::User);
    }
}
