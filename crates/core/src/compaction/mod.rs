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
    ) -> Result<CompactedContext>;

    fn estimate_tokens(&self, messages: &[ChatMessage]) -> usize;
}

pub type Result<T> = std::result::Result<T, CompactionError>;

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
