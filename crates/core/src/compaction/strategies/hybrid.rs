use async_trait::async_trait;

use crate::compaction::strategies::SummaryStrategy;
use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::ChatMessage;
use crate::provider::Provider;

pub struct HybridStrategy;

#[async_trait]
impl CompactionStrategy for HybridStrategy {
    async fn compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
        provider: Option<&dyn Provider>,
        model: &str,
    ) -> Result<CompactedContext> {
        SummaryStrategy
            .compact(messages, context_limit, config, provider, model)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{MessageContent, Role};

    fn make_message(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn test_hybrid_falls_back_to_trim_without_provider() {
        let mut messages = vec![make_message(Role::System, "You are a helpful assistant.")];
        for i in 0..50 {
            messages.push(make_message(Role::User, &format!("Message number {}", i)));
        }

        let strategy = HybridStrategy;
        let config = CompactionConfig {
            keep_last: 5,
            ..Default::default()
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(strategy.compact(&messages, 100_000, &config, None, "test-model"))
            .unwrap();

        assert!(result.compacted_count < result.original_count);
    }

    #[test]
    fn test_hybrid_summarizes_short_history_without_losing_system_prompt() {
        let messages = vec![
            make_message(Role::System, "You are a helpful assistant."),
            make_message(Role::User, "first request"),
            make_message(Role::Assistant, "first answer"),
            make_message(Role::User, "latest request"),
        ];
        let strategy = HybridStrategy;
        let config = CompactionConfig::default();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(strategy.compact(&messages, 100_000, &config, None, "test-model"))
            .unwrap();

        assert!(result
            .messages
            .iter()
            .any(|message| message_text(message) == "You are a helpful assistant."));
        assert_eq!(
            result
                .messages
                .iter()
                .filter(
                    |message| message_text(message).starts_with("[Previous conversation summary]")
                )
                .count(),
            1
        );
    }

    fn message_text(message: &ChatMessage) -> String {
        match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(_) => String::new(),
        }
    }
}
