use async_trait::async_trait;

use crate::compaction::{CompactedContext, CompactionConfig, CompactionStrategy, Result};
use crate::provider::types::ChatMessage;

pub struct TrimStrategy;

#[async_trait]
impl CompactionStrategy for TrimStrategy {
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
