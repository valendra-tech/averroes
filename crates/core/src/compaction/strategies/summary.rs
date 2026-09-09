use async_trait::async_trait;

use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::{ChatMessage, ContentPart, MessageContent, Role};
use crate::provider::{ChatRequest, Provider};

pub struct SummaryStrategy;

const MAX_SUMMARY_INPUT_CHARS: usize = 64_000;
const MAX_SUMMARY_OUTPUT_CHARS: usize = 8_000;
const SUMMARY_TRUNCATION_MARKER: &str = "\n[…older context omitted from summary input…]\n";
pub(crate) const SUMMARY_MARKER: &str = "[Previous conversation summary]";

fn message_text(msg: &ChatMessage) -> String {
    match &msg.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                crate::provider::types::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn summary_message_text(message: &ChatMessage) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.clone()),
                ContentPart::ToolResult { content, .. } => Some(content.clone()),
                ContentPart::Image { source } => {
                    Some(format!("[image omitted: media_type={}]", source.media_type))
                }
                ContentPart::ToolUse { name, input, .. } => {
                    Some(format!("tool_use name={name} input={input}"))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn is_summary_message(message: &ChatMessage) -> bool {
    message_text(message).starts_with(SUMMARY_MARKER)
}

fn format_summary_message(index: usize, message: &ChatMessage) -> String {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let call_id = message
        .tool_call_id
        .as_deref()
        .map(|id| format!(" tool_call_id={id}"))
        .unwrap_or_default();
    let mut output = format!("[message {index} role={role}{call_id}]\n");
    let text = summary_message_text(message);
    if !text.is_empty() {
        output.push_str("text: ");
        output.push_str(&text);
        output.push('\n');
    }
    if let Some(tool_calls) = &message.tool_calls {
        for call in tool_calls {
            output.push_str(&format!(
                "tool_call id={} name={} arguments={}\n",
                call.id, call.function.name, call.function.arguments
            ));
        }
    }
    output
}

fn bounded_summary_input(messages: &[ChatMessage], previous_context: Option<&str>) -> String {
    let mut sections = Vec::new();
    if let Some(previous_context) = previous_context.filter(|context| !context.is_empty()) {
        sections.push(format!(
            "[existing compacted context]\n{previous_context}\n"
        ));
    }
    sections.extend(
        messages
            .iter()
            .enumerate()
            .filter(|(_, message)| !is_summary_message(message))
            .map(|(index, message)| format_summary_message(index, message)),
    );
    bound_summary_text(&sections.join("\n"))
}

fn bound_summary_text(text: &str) -> String {
    let max_chars = MAX_SUMMARY_INPUT_CHARS;
    let text_chars = text.chars().count();
    if max_chars == 0 {
        return String::new();
    }
    if text_chars <= max_chars {
        return text.to_owned();
    }

    let marker_len = SUMMARY_TRUNCATION_MARKER.chars().count();
    if max_chars <= marker_len + 2 {
        return text.chars().take(max_chars).collect();
    }

    let chars = text.chars().collect::<Vec<_>>();
    let available = max_chars - marker_len;
    let head_len = available / 2;
    let tail_len = available - head_len;
    let head = chars[..head_len].iter().collect::<String>();
    let tail = chars[chars.len() - tail_len..].iter().collect::<String>();
    format!("{head}{SUMMARY_TRUNCATION_MARKER}{tail}")
}

async fn generate_summary(
    provider: &dyn Provider,
    model: &str,
    messages: &[ChatMessage],
    previous_context: Option<&str>,
) -> std::result::Result<String, crate::compaction::CompactionError> {
    let text = bounded_summary_input(messages, previous_context);
    let request = ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(format!(
                "Rewrite the following conversation into a compact understood context.\n\nReturn only these sections, with concise factual bullets:\nObjective:\nDecisions:\nConstraints:\nActive skills and instructions:\nTool findings:\nOpen questions:\nCurrent state:\nNext action:\n\nPreserve the names and essential rules of skills already loaded, plus durable facts learned from tools, without copying raw tool payloads. Do not reproduce the transcript or hidden reasoning. Do not invent missing facts. Keep it under 1,200 words.\n\n{}",
                text
            )),
            tool_call_id: None,
            tool_calls: None,
        }],
        tools: vec![],
        temperature: Some(0.3),
        system: Some(
            "You are the conversation context editor. Preserve only useful state for continuing the work.".into(),
        ),
        reasoning_effort: None,
    };

    let response = provider.chat(request).await?;
    Ok(message_text(&response.message))
}

#[async_trait]
impl CompactionStrategy for SummaryStrategy {
    async fn compact(
        &self,
        messages: &[ChatMessage],
        _context_limit: usize,
        _config: &CompactionConfig,
        provider: Option<&dyn Provider>,
        model: &str,
    ) -> Result<CompactedContext> {
        let original_count = messages.len();
        let system_messages = messages
            .iter()
            .filter(|message| message.role == Role::System && !is_summary_message(message))
            .cloned()
            .collect::<Vec<_>>();
        let previous_context = messages
            .iter()
            .filter(|message| is_summary_message(message))
            .map(|message| {
                message_text(message)
                    .strip_prefix(SUMMARY_MARKER)
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
            .filter(|summary| !summary.is_empty())
            .collect::<Vec<_>>();
        let conversation = messages
            .iter()
            .filter(|message| message.role != Role::System)
            .cloned()
            .collect::<Vec<_>>();
        let keep_last = _config.keep_last.min(conversation.len());
        if conversation.len() <= keep_last {
            return Ok(CompactedContext {
                messages: messages.to_vec(),
                original_count,
                compacted_count: messages.len(),
            });
        }

        let mut split_idx = conversation.len() - keep_last;
        while split_idx > 0 && conversation[split_idx].role == Role::Tool {
            split_idx -= 1;
        }
        let older = &conversation[..split_idx];
        let recent = &conversation[split_idx..];
        if older.is_empty() {
            return Ok(CompactedContext {
                messages: messages.to_vec(),
                original_count,
                compacted_count: messages.len(),
            });
        }
        let previous_context =
            (!previous_context.is_empty()).then(|| previous_context.join("\n\n"));

        let summary_text = if let Some(provider) = provider {
            generate_summary(provider, model, older, previous_context.as_deref()).await?
        } else {
            bounded_summary_input(older, previous_context.as_deref())
        };
        let summary_text = bounded_summary_output(&summary_text);

        let summary_msg = ChatMessage {
            role: Role::System,
            content: MessageContent::Text(format!(
                "[Previous conversation summary]\n\n{}",
                summary_text
            )),
            tool_call_id: None,
            tool_calls: None,
        };

        let mut compacted = system_messages;
        compacted.push(summary_msg);
        compacted.extend(recent.iter().cloned());
        let compacted = crate::compaction::sanitize_tool_history(compacted);

        Ok(CompactedContext {
            compacted_count: compacted.len(),
            messages: compacted,
            original_count,
        })
    }
}

fn bounded_summary_output(summary: &str) -> String {
    let summary = summary.trim();
    if summary.chars().count() <= MAX_SUMMARY_OUTPUT_CHARS {
        return summary.to_owned();
    }
    let mut output = summary
        .chars()
        .take(MAX_SUMMARY_OUTPUT_CHARS)
        .collect::<String>();
    output.push_str("\n[…context summary truncated…]");
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FunctionCall, ToolCall};
    use crate::provider::{ChatResponse, ChatStream, ProviderError};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct CapturingProvider {
        requested_model: Arc<Mutex<Option<String>>>,
        requested_prompt: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl Provider for CapturingProvider {
        async fn chat(&self, request: ChatRequest) -> crate::provider::Result<ChatResponse> {
            *self.requested_model.lock().unwrap() = Some(request.model);
            *self.requested_prompt.lock().unwrap() = Some(match &request.messages[0].content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Parts(_) => "<parts>".into(),
            });
            Ok(ChatResponse {
                message: ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text("summary".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                reasoning: None,
                stop_reason: None,
            })
        }

        async fn chat_stream(&self, _request: ChatRequest) -> crate::provider::Result<ChatStream> {
            Err(ProviderError::Other("stream unused in test".into()))
        }

        fn context_window(&self, _model: &str) -> usize {
            100_000
        }

        fn supports_tools(&self, _model: &str) -> bool {
            false
        }

        fn default_model(&self) -> &str {
            "provider-default"
        }
    }

    #[tokio::test]
    async fn summary_uses_selected_model() {
        let requested_model = Arc::new(Mutex::new(None));
        let provider = CapturingProvider {
            requested_model: requested_model.clone(),
            requested_prompt: Arc::new(Mutex::new(None)),
        };
        let messages = vec![
            ChatMessage {
                role: Role::System,
                content: MessageContent::Text("system".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Role::User,
                content: MessageContent::Text("middle".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("last".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        SummaryStrategy
            .compact(
                &messages,
                100_000,
                &CompactionConfig {
                    keep_last: 1,
                    ..Default::default()
                },
                Some(&provider),
                "selected-model",
            )
            .await
            .unwrap();

        assert_eq!(
            requested_model.lock().unwrap().as_deref(),
            Some("selected-model")
        );
    }

    #[tokio::test]
    async fn summary_input_preserves_roles_and_tool_metadata() {
        let requested_prompt = Arc::new(Mutex::new(None));
        let provider = CapturingProvider {
            requested_model: Arc::new(Mutex::new(None)),
            requested_prompt: requested_prompt.clone(),
        };
        let messages = vec![
            ChatMessage {
                role: Role::System,
                content: MessageContent::Text("system instructions".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Role::User,
                content: MessageContent::Text("inspect the project".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("I will inspect it".into()),
                tool_call_id: None,
                tool_calls: Some(vec![crate::provider::types::ToolCall {
                    id: "call-1".into(),
                    call_type: "function".into(),
                    function: crate::provider::types::FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"README.md"}"#.into(),
                    },
                }]),
            },
            ChatMessage {
                role: Role::Tool,
                content: MessageContent::Text("README contents".into()),
                tool_call_id: Some("call-1".into()),
                tool_calls: None,
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("The project uses Rust".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        SummaryStrategy
            .compact(
                &messages,
                100_000,
                &CompactionConfig {
                    keep_last: 1,
                    ..Default::default()
                },
                Some(&provider),
                "selected-model",
            )
            .await
            .unwrap();

        let prompt = requested_prompt.lock().unwrap().clone().unwrap();
        assert!(prompt.contains("role=user"));
        assert!(prompt.contains("role=assistant"));
        assert!(prompt.contains("tool_call id=call-1 name=read_file"));
        assert!(prompt.contains(r#"arguments={"path":"README.md"}"#));
        assert!(prompt.contains("role=tool tool_call_id=call-1"));
        assert!(prompt.contains("README contents"));
    }

    fn make_message(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[tokio::test]
    async fn summary_replaces_previous_summary_instead_of_accumulating_markers() {
        let messages = vec![
            make_message(Role::System, "system"),
            make_message(
                Role::System,
                "[Previous conversation summary]\n\nold objective",
            ),
            make_message(Role::User, "old request"),
            make_message(Role::Assistant, "old answer"),
            make_message(Role::User, "latest request"),
        ];

        let result = SummaryStrategy
            .compact(
                &messages,
                100_000,
                &CompactionConfig {
                    keep_last: 1,
                    ..Default::default()
                },
                None,
                "test-model",
            )
            .await
            .unwrap();

        assert_eq!(
            result
                .messages
                .iter()
                .filter(|message| message_text(message).starts_with(SUMMARY_MARKER))
                .count(),
            1
        );
        assert!(result
            .messages
            .iter()
            .any(|message| message_text(message) == "latest request"));
        assert!(result
            .messages
            .iter()
            .any(|message| message_text(message) == "system"));
    }

    #[tokio::test]
    async fn summary_keeps_assistant_tool_pair_when_tail_starts_at_result() {
        let messages = vec![
            make_message(Role::System, "system"),
            make_message(Role::User, "old request"),
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("calling read".into()),
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                }]),
            },
            ChatMessage {
                role: Role::Tool,
                content: MessageContent::Text("tool result".into()),
                tool_call_id: Some("call-1".into()),
                tool_calls: None,
            },
        ];
        let result = SummaryStrategy
            .compact(
                &messages,
                100_000,
                &CompactionConfig {
                    keep_last: 1,
                    ..Default::default()
                },
                None,
                "test-model",
            )
            .await
            .unwrap();

        assert!(result
            .messages
            .iter()
            .any(|message| { message.role == Role::Assistant && message.tool_calls.is_some() }));
        assert!(result.messages.iter().any(|message| {
            message.role == Role::Tool && message.tool_call_id.as_deref() == Some("call-1")
        }));
    }
}
