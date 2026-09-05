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
use std::future::Future;
use std::time::Duration;
use types::TokenUsage;

const HTTP_REQUEST_MAX_RETRIES: usize = 3;
const HTTP_REQUEST_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const HTTP_REQUEST_MAX_BACKOFF: Duration = Duration::from_secs(4);

pub(crate) async fn send_with_retry<F, Fut>(request: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<reqwest::Response, reqwest::Error>>,
{
    send_with_retry_with_policy(
        request,
        HTTP_REQUEST_MAX_RETRIES,
        HTTP_REQUEST_INITIAL_BACKOFF,
        HTTP_REQUEST_MAX_BACKOFF,
    )
    .await
}

async fn send_with_retry_with_policy<F, Fut>(
    mut request: F,
    max_retries: usize,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<reqwest::Response, reqwest::Error>>,
{
    for attempt in 0..=max_retries {
        match request().await {
            Ok(response)
                if attempt < max_retries && is_retryable_http_status(response.status()) =>
            {
                drop(response);
                sleep_before_retry(attempt, max_retries, initial_backoff, max_backoff).await;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt < max_retries && is_retryable_http_error(&error) => {
                sleep_before_retry(attempt, max_retries, initial_backoff, max_backoff).await;
            }
            Err(error) => return Err(ProviderError::Http(error)),
        }
    }

    unreachable!("HTTP retry loop always returns")
}

fn is_retryable_http_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || error.is_request()
}

fn is_retryable_http_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::TOO_EARLY
            | reqwest::StatusCode::TOO_MANY_REQUESTS
            | reqwest::StatusCode::INTERNAL_SERVER_ERROR
            | reqwest::StatusCode::BAD_GATEWAY
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
            | reqwest::StatusCode::GATEWAY_TIMEOUT
    )
}

pub(crate) fn retry_backoff(
    attempt: usize,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> Duration {
    let shift = attempt.min(127) as u32;
    let multiplier = 1u128.checked_shl(shift).unwrap_or(u128::MAX);
    let millis = initial_backoff
        .as_millis()
        .saturating_mul(multiplier)
        .min(max_backoff.as_millis());
    Duration::from_millis(millis.min(u64::MAX as u128) as u64)
}

async fn sleep_before_retry(
    attempt: usize,
    max_retries: usize,
    initial_backoff: Duration,
    max_backoff: Duration,
) {
    let delay = retry_backoff(attempt, initial_backoff, max_backoff);
    crate::observability::diagnostics::record(
        crate::observability::diagnostics::DiagnosticLevel::Warning,
        "provider.http",
        format!(
            "Transient provider request failure; retrying in {} ms ({}/{})",
            delay.as_millis(),
            attempt + 1,
            max_retries
        ),
    );
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

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

#[cfg(test)]
mod retry_tests {
    use super::send_with_retry_with_policy;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn retries_transient_http_statuses_with_exponential_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).await.unwrap();
                let status = if attempt < 2 { 503 } else { 200 };
                let body = "ok";
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{body}"
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let client = reqwest::Client::new();
        let response = send_with_retry_with_policy(
            || client.get(format!("http://{address}/responses")).send(),
            3,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retries_transient_transport_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).await.unwrap();
                if attempt == 0 {
                    drop(stream);
                    continue;
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .unwrap();
            }
        });

        let client = reqwest::Client::new();
        let response = send_with_retry_with_policy(
            || client.get(format!("http://{address}/responses")).send(),
            3,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        server.await.unwrap();
    }

    #[test]
    fn retry_backoff_is_exponential_and_capped() {
        assert_eq!(
            super::retry_backoff(0, Duration::from_millis(100), Duration::from_secs(2)),
            Duration::from_millis(100)
        );
        assert_eq!(
            super::retry_backoff(1, Duration::from_millis(100), Duration::from_secs(2)),
            Duration::from_millis(200)
        );
        assert_eq!(
            super::retry_backoff(8, Duration::from_millis(100), Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }
}
