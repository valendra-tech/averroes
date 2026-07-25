pub mod types;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use types::TokenUsage;

pub use crate::provider::types::ChatMessage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub system: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Option<TokenUsage>,
    pub stop_reason: Option<String>,
}

pub type ChatStream = Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    TextDelta { text: String },
    ToolCallDelta { id: String, name: String, arguments_delta: String },
    ToolCallEnd { id: String },
    MessageStart { message: ChatMessage },
    MessageEnd { usage: Option<TokenUsage> },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream>;
    fn context_window(&self, model: &str) -> usize;
    fn supports_tools(&self, model: &str) -> bool;
    fn default_model(&self) -> &str;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error: status={status}, body={body}")]
    Api { status: u16, body: String },
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("Rate limited")]
    RateLimited,
    #[error("Context window exceeded: {used}/{limit}")]
    ContextExceeded { used: usize, limit: usize },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ProviderError>;
