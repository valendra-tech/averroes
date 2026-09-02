pub mod strategies;

use crate::provider::types::{ChatMessage, ContentPart, MessageContent, Role};
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
    async fn compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
        provider: Option<&dyn Provider>,
        model: &str,
    ) -> Result<CompactedContext>;
}

pub type Result<T> = std::result::Result<T, CompactionError>;

/// Tool results remain useful to the model, but unbounded shell/file output
/// can dominate every following request. Keep a generous live limit and a
/// tighter post-compaction limit; tools can always be rerun with a narrower
/// query when the omitted middle matters.
pub const MAX_LIVE_TOOL_OUTPUT_BYTES: usize = 50 * 1024;
pub const MAX_LIVE_TOOL_OUTPUT_LINES: usize = 2_000;
const MAX_COMPACTED_TOOL_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_COMPACTED_TOOL_OUTPUT_LINES: usize = 600;

pub fn bound_live_tool_output(content: &str) -> String {
    bound_tool_output(
        content,
        MAX_LIVE_TOOL_OUTPUT_BYTES,
        MAX_LIVE_TOOL_OUTPUT_LINES,
    )
}

pub fn compact_tool_outputs(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .map(|mut message| {
            if message.role != Role::Tool {
                return message;
            }
            message.content = match message.content {
                MessageContent::Text(content) => MessageContent::Text(bound_tool_output(
                    &content,
                    MAX_COMPACTED_TOOL_OUTPUT_BYTES,
                    MAX_COMPACTED_TOOL_OUTPUT_LINES,
                )),
                MessageContent::Parts(parts) => MessageContent::Parts(
                    parts
                        .into_iter()
                        .map(|part| match part {
                            ContentPart::ToolResult {
                                tool_use_id,
                                content,
                            } => ContentPart::ToolResult {
                                tool_use_id,
                                content: bound_tool_output(
                                    &content,
                                    MAX_COMPACTED_TOOL_OUTPUT_BYTES,
                                    MAX_COMPACTED_TOOL_OUTPUT_LINES,
                                ),
                            },
                            other => other,
                        })
                        .collect(),
                ),
            };
            message
        })
        .collect()
}

fn bound_tool_output(content: &str, max_bytes: usize, max_lines: usize) -> String {
    let line_count = content.lines().count();
    if content.len() <= max_bytes && line_count <= max_lines {
        return content.to_string();
    }

    let marker = format!(
        "\n\n[… tool output truncated from {} bytes and {} lines; rerun the tool with narrower arguments to inspect the omitted middle …]\n\n",
        content.len(),
        line_count
    );
    if marker.len() >= max_bytes || max_lines < 3 {
        return truncate_prefix(&marker, max_bytes);
    }

    let available = max_bytes - marker.len();
    let head_bytes = available / 2;
    let tail_bytes = available - head_bytes;
    // The marker deliberately has surrounding blank lines; reserve enough
    // line budget for it so line-heavy command output cannot exceed the cap.
    let content_lines = max_lines.saturating_sub(6);
    let head_lines = content_lines / 2;
    let tail_lines = content_lines - head_lines;
    let head = prefix_fragment(content, head_bytes, head_lines);
    let tail = suffix_fragment(content, tail_bytes, tail_lines);
    format!("{head}{marker}{tail}")
}

fn prefix_fragment(content: &str, max_bytes: usize, max_lines: usize) -> String {
    let mut fragment = String::new();
    for line in content.split_inclusive('\n').take(max_lines) {
        let remaining = max_bytes.saturating_sub(fragment.len());
        if remaining == 0 {
            break;
        }
        fragment.push_str(&truncate_prefix(line, remaining));
        if line.len() > remaining {
            break;
        }
    }
    fragment
}

fn suffix_fragment(content: &str, max_bytes: usize, max_lines: usize) -> String {
    let mut lines = content
        .split_inclusive('\n')
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>();
    lines.reverse();
    truncate_suffix(&lines.concat(), max_bytes)
}

fn truncate_prefix(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn truncate_suffix(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut start = value.len() - max_bytes;
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_string()
}

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

    #[test]
    fn live_tool_output_is_bounded_by_bytes_and_keeps_both_ends() {
        let content = format!("START\n{}END", "0123456789\n".repeat(8_000));

        let bounded = bound_live_tool_output(&content);

        assert!(bounded.len() <= MAX_LIVE_TOOL_OUTPUT_BYTES);
        assert!(bounded.lines().count() <= MAX_LIVE_TOOL_OUTPUT_LINES);
        assert!(bounded.starts_with("START"));
        assert!(bounded.ends_with("END"));
        assert!(bounded.contains("tool output truncated"));
    }

    #[test]
    fn compacted_tool_results_receive_the_tighter_limit() {
        let call = ToolCall {
            id: "call-large".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "bash".into(),
                arguments: "{}".into(),
            },
        };
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(String::new()),
                tool_call_id: None,
                tool_calls: Some(vec![call.clone()]),
            },
            ChatMessage {
                role: Role::Tool,
                content: MessageContent::Text("large output\n".repeat(10_000)),
                tool_call_id: Some(call.id),
                tool_calls: None,
            },
        ];

        let compacted = compact_tool_outputs(messages);
        let MessageContent::Text(output) = &compacted[1].content else {
            panic!("expected text tool output");
        };
        assert!(output.len() <= MAX_COMPACTED_TOOL_OUTPUT_BYTES);
        assert!(output.contains("tool output truncated"));
    }
}
