use async_trait::async_trait;

use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::{ChatMessage, MessageContent};

pub struct TrimStrategy;

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

#[async_trait]
impl CompactionStrategy for TrimStrategy {
    fn should_compact(
        &self,
        messages: &[ChatMessage],
        context_limit: usize,
        config: &CompactionConfig,
    ) -> bool {
        let estimated = self.estimate_tokens(messages);
        estimated > (config.threshold * context_limit as f64) as usize
            && messages.len() > config.keep_last + 2
    }

    async fn compact(
        &self,
        messages: &[ChatMessage],
        _context_limit: usize,
        config: &CompactionConfig,
        _provider: Option<&dyn crate::provider::Provider>,
        _model: &str,
    ) -> Result<CompactedContext> {
        let original_count = messages.len();
        let keep = config.keep_last.min(messages.len());
        let compacted: Vec<ChatMessage> = messages[messages.len() - keep..].to_vec();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{ChatMessage, MessageContent, Role};

    fn make_message(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn test_trim_compaction() {
        let mut messages = vec![make_message(Role::System, "You are a helpful assistant.")];
        for i in 0..50 {
            messages.push(make_message(Role::User, &format!("Message number {}", i)));
        }

        let strategy = TrimStrategy;
        let config = CompactionConfig {
            keep_last: 5,
            ..Default::default()
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(strategy.compact(&messages, 100_000, &config, None, "test-model"))
            .unwrap();

        assert_eq!(result.original_count, 51);
        assert_eq!(result.compacted_count, 5);
    }
}
