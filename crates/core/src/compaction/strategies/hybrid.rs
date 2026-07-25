use async_trait::async_trait;

use crate::compaction::strategies::{SummaryStrategy, TrimStrategy};
use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::ChatMessage;
use crate::provider::Provider;

pub struct HybridStrategy;

#[async_trait]
impl CompactionStrategy for HybridStrategy {
    fn should_compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
    ) -> bool {
        TrimStrategy.should_compact(messages, context_limit, config)
    }

    async fn compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
        provider: Option<&dyn Provider>,
    ) -> Result<CompactedContext> {
        let original_count = messages.len();

        if messages.len() <= config.keep_last + 2 {
            return TrimStrategy
                .compact(messages, context_limit, config, provider)
                .await;
        }

        let split_idx = messages.len() - config.keep_last;
        let old = &messages[..split_idx];
        let recent = &messages[split_idx..];

        let summary_result = SummaryStrategy
            .compact(old, context_limit, config, provider)
            .await?;

        let mut compacted = summary_result.messages;
        compacted.extend(recent.iter().cloned());

        Ok(CompactedContext {
            compacted_count: compacted.len(),
            messages: compacted,
            original_count,
        })
    }

    fn estimate_tokens(&self, messages: &[ChatMessage]) -> usize {
        TrimStrategy.estimate_tokens(messages)
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
            .block_on(strategy.compact(&messages, 100_000, &config, None))
            .unwrap();

        assert!(result.compacted_count < result.original_count);
    }
}
