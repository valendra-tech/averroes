use async_trait::async_trait;

use crate::compaction::{
    sanitize_tool_history, CompactedContext, CompactionConfig, CompactionStrategy, Result,
};
use crate::provider::types::{ChatMessage, Role};

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
        let system_messages = messages
            .iter()
            .filter(|message| message.role == Role::System)
            .cloned()
            .collect::<Vec<_>>();
        let recent_messages = messages
            .iter()
            .filter(|message| message.role != Role::System)
            .cloned()
            .collect::<Vec<_>>();
        let keep = config.keep_last.min(recent_messages.len());
        let mut start = recent_messages.len().saturating_sub(keep);
        while start > 0 && recent_messages[start].role == Role::Tool {
            start -= 1;
        }
        let mut compacted = system_messages;
        compacted.extend(recent_messages[start..].iter().cloned());
        let compacted = sanitize_tool_history(compacted);

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
        assert_eq!(result.compacted_count, 6);
        assert!(result.messages.iter().any(|message| {
                matches!(&message.content, MessageContent::Text(text) if text == "You are a helpful assistant.")
            }));
    }
}
