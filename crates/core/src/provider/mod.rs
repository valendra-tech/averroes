pub mod anthropic;
pub mod codex;
pub mod factory;
pub mod generic;
pub mod hooks;
pub mod models;
pub mod openai;
pub mod qdivzero;
pub(crate) mod reasoning;
pub mod types;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use types::TokenUsage;

pub use crate::provider::types::ChatMessage;
pub use hooks::{ModelDiscovery, ProviderHook, ProviderRegistry, StandardProviderHook};
pub use models::{
    curated_embedding_models, curated_models, filter_models, merge_catalog, merge_live_catalog,
    model_uses_reasoning_api, model_uses_responses_api, parse_provider_models, provider_label,
    ModelCapabilities, ModelInfo, ModelSource, ProviderModel, ProviderModelKind,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub temperature: Option<f32>,
    pub system: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    /// Provider-returned reasoning summary/thinking text. This is optional
    /// because some providers expose it only through streaming events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    pub usage: Option<TokenUsage>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub embeddings: Vec<Vec<f32>>,
}

pub type ChatStream = Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallDelta {
        id: String,
        name: String,
        arguments_delta: String,
    },
    ToolCallEnd {
        id: String,
    },
    MessageStart {
        message: ChatMessage,
    },
    MessageEnd {
        usage: Option<TokenUsage>,
    },
    Error {
        message: String,
    },
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
    async fn list_models(&self) -> Result<Vec<ProviderModel>> {
        Ok(Vec::new())
    }
    async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        Err(ProviderError::Other(
            "this provider does not expose embeddings".into(),
        ))
    }
    fn context_window(&self, model: &str) -> usize;
    fn supports_tools(&self, model: &str) -> bool;
    fn default_model(&self) -> &str;
    fn provider_name(&self) -> &'static str {
        "generic"
    }
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

pub fn log_debug_request(_request: &ChatRequest, _provider_name: &str) {
    // Request bodies can contain user code and credentials passed to tools.
    // Averroes deliberately never enables request logging through environment
    // variables; diagnostics must be added explicitly at a safe call site.
}

pub(crate) fn parse_openai_embeddings(value: &Value) -> Result<EmbeddingResponse> {
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return Err(ProviderError::Other(
            "embedding response has no data array".into(),
        ));
    };
    let mut indexed = data
        .iter()
        .filter_map(|item| {
            let values = item.get("embedding")?.as_array()?;
            let embedding = values
                .iter()
                .map(|value| value.as_f64().map(|value| value as f32))
                .collect::<Option<Vec<_>>>()?;
            Some((
                item.get("index").and_then(Value::as_u64).unwrap_or(0),
                embedding,
            ))
        })
        .collect::<Vec<_>>();
    indexed.sort_by_key(|(index, _)| *index);
    if indexed.is_empty() {
        return Err(ProviderError::Other(
            "embedding response contains no usable vectors".into(),
        ));
    }
    Ok(EmbeddingResponse {
        embeddings: indexed
            .into_iter()
            .map(|(_, embedding)| embedding)
            .collect(),
    })
}
