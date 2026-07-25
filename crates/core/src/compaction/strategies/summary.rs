use async_trait::async_trait;

use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::{ChatMessage, MessageContent, Role};
use crate::provider::{ChatRequest, Provider};

pub struct SummaryStrategy;

fn rough_token_count(msg: &ChatMessage) -> usize {
    match &msg.content {
        MessageContent::Text(t) => t.len() / 4,
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                crate::provider::types::ContentPart::Text { text } => text.len() / 4,
                _ => 200,
            })
            .sum(),
    }
}

fn message_text(msg: &ChatMessage) -> String {
    match &msg.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => {
            parts
                .iter()
                .filter_map(|p| match p {
                    crate::provider::types::ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
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
    messages: &[ChatMessage],
) -> std::result::Result<String, crate::compaction::CompactionError> {
    let text = fallback_concat(messages);
    let request = ChatRequest {
        model: provider.default_model().to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(format!(
                "Please provide a concise summary of the following conversation:\n\n{}",
                text
            )),
            tool_call_id: None,
            tool_calls: None,
        }],
        tools: vec![],
        max_tokens: None,
        temperature: Some(0.3),
        system: Some("You are a summarization assistant. Produce a brief, accurate summary.".into()),
    };

    let response = provider.chat(request).await?;
    Ok(message_text(&response.message))
}

#[async_trait]
impl CompactionStrategy for SummaryStrategy {
    fn should_compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
    ) -> bool {
        let estimated = self.estimate_tokens(messages);
        estimated > (config.threshold * context_limit as f64) as usize && messages.len() > 2
    }

    async fn compact(
        &self,
        messages: &[ChatMessage],
        _context_limit: usize,
        _config: &CompactionConfig,
        provider: Option<&dyn Provider>,
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
            generate_summary(provider, middle).await?
        } else {
            fallback_concat(middle)
        };

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

    fn estimate_tokens(&self, messages: &[ChatMessage]) -> usize {
        messages.iter().map(rough_token_count).sum()
    }
}
