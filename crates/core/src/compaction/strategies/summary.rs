use async_trait::async_trait;

use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::{ChatMessage, MessageContent, Role};
use crate::provider::{ChatRequest, Provider};

pub struct SummaryStrategy;

const MAX_SUMMARY_INPUT_CHARS: usize = 64_000;
const MAX_SUMMARY_OUTPUT_CHARS: usize = 8_000;
const SUMMARY_TRUNCATION_MARKER: &str = "\n[…older context omitted from summary input…]\n";

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

fn fallback_concat(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| message_text(m))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn generate_summary(
    provider: &dyn Provider,
    model: &str,
    messages: &[ChatMessage],
) -> std::result::Result<String, crate::compaction::CompactionError> {
    let text = bounded_summary_input(messages);
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

fn bounded_summary_input(messages: &[ChatMessage]) -> String {
    let text = fallback_concat(messages);
    let max_chars = MAX_SUMMARY_INPUT_CHARS;
    let text_chars = text.chars().count();
    if max_chars == 0 {
        return String::new();
    }
    if text_chars <= max_chars {
        return text;
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

        if messages.len() <= 2 {
            return Ok(CompactedContext {
                messages: messages.to_vec(),
                original_count,
                compacted_count: messages.len(),
            });
        }

        let first = &messages[0];
        let last = &messages[messages.len() - 1];
        let middle = &messages[1..messages.len() - 1];

        let summary_text = if let Some(provider) = provider {
            generate_summary(provider, model, middle).await?
        } else {
            fallback_concat(middle)
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

        let compacted = vec![first.clone(), summary_msg, last.clone()];

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
    use crate::provider::{ChatResponse, ChatStream, ProviderError};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct CapturingProvider {
        requested_model: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl Provider for CapturingProvider {
        async fn chat(&self, request: ChatRequest) -> crate::provider::Result<ChatResponse> {
            *self.requested_model.lock().unwrap() = Some(request.model);
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
                &CompactionConfig::default(),
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
}
