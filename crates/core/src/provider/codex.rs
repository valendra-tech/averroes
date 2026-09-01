use super::openai::{spawn_responses_stream_producer, OpenAiProvider, OpenAiStream};
use super::types::{ChatMessage, FunctionCall, MessageContent, Role, ToolCall};
use super::{
    log_debug_request, parse_openai_embeddings, ChatRequest, ChatResponse, ChatStream,
    EmbeddingRequest, EmbeddingResponse, Provider, ProviderError, ProviderModel, Result,
    StreamEvent,
};
use crate::codex::{CodexClient, CODEX_API_BASE, CODEX_CLIENT_VERSION, CODEX_ORIGINATOR};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;

use crate::connection::ConnectionKind;
use crate::provider::hooks::{ModelDiscovery, ProviderRegistry, StandardProviderHook};

pub(crate) fn register_provider_hook(registry: &mut ProviderRegistry) {
    registry.register(StandardProviderHook::new(
        "codex",
        ConnectionKind::Codex,
        Some("openai"),
        ModelDiscovery::CodexAccount,
    ));
}

pub struct ChatGptCodexProvider {
    codex: Arc<CodexClient>,
    http: reqwest::Client,
    default_model: String,
}

impl ChatGptCodexProvider {
    pub fn new(codex: Arc<CodexClient>, default_model: impl Into<String>) -> Self {
        Self {
            codex,
            http: reqwest::Client::builder()
                .user_agent(format!("{CODEX_ORIGINATOR}/{CODEX_CLIENT_VERSION}"))
                .build()
                .unwrap_or_default(),
            default_model: default_model.into(),
        }
    }

    fn request_body(request: &ChatRequest, stream: bool) -> Value {
        let mut body = OpenAiProvider::build_responses_request(request, stream);
        if let Some(object) = body.as_object_mut() {
            // ChatGPT's Codex endpoint follows the first-party Responses shape.
            // Output limits are selected by the provider, while state remains
            // local to Averroes and tool calls may run concurrently.
            object.insert("tool_choice".into(), Value::String("auto".into()));
            object.insert("parallel_tool_calls".into(), Value::Bool(true));
            object.insert("store".into(), Value::Bool(false));
            object.insert(
                "include".into(),
                serde_json::json!(["reasoning.encrypted_content"]),
            );
        }
        body
    }

    async fn send(&self, body: &Value) -> Result<reqwest::Response> {
        let response = self.send_once(body).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.codex
                .refresh()
                .await
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            return self.send_once(body).await;
        }
        Ok(response)
    }

    async fn send_once(&self, body: &Value) -> Result<reqwest::Response> {
        let credentials = self
            .codex
            .credentials()
            .await
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        Ok(self
            .http
            .post(format!("{CODEX_API_BASE}/responses"))
            .bearer_auth(credentials.access_token)
            .header("ChatGPT-Account-ID", credentials.account_id)
            .header("originator", CODEX_ORIGINATOR)
            .header("Accept", "text/event-stream")
            .json(body)
            .send()
            .await?)
    }

    async fn send_embeddings(&self, body: &Value) -> Result<reqwest::Response> {
        let response = self.send_embeddings_once(body).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.codex
                .refresh()
                .await
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            return self.send_embeddings_once(body).await;
        }
        Ok(response)
    }

    async fn send_embeddings_once(&self, body: &Value) -> Result<reqwest::Response> {
        let credentials = self
            .codex
            .credentials()
            .await
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        Ok(self
            .http
            .post(format!("{CODEX_API_BASE}/embeddings"))
            .bearer_auth(credentials.access_token)
            .header("ChatGPT-Account-ID", credentials.account_id)
            .header("originator", CODEX_ORIGINATOR)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?)
    }
}

#[async_trait]
impl Provider for ChatGptCodexProvider {
    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let response = self
            .send_embeddings(&serde_json::json!({
                "model": request.model,
                "input": request.input,
            }))
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }
        parse_openai_embeddings(&response.json().await?)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let mut stream = self.chat_stream(request).await?;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::<ToolCall>::new();
        let mut usage = None;

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
                StreamEvent::ToolCallDelta {
                    id,
                    name,
                    arguments_delta,
                } => {
                    if let Some(call) = tool_calls.iter_mut().find(|call| call.id == id) {
                        if !name.is_empty() {
                            call.function.name = name;
                        }
                        call.function.arguments.push_str(&arguments_delta);
                    } else {
                        tool_calls.push(ToolCall {
                            id,
                            call_type: "function".into(),
                            function: FunctionCall {
                                name,
                                arguments: arguments_delta,
                            },
                        });
                    }
                }
                StreamEvent::MessageEnd {
                    usage: stream_usage,
                } => {
                    usage = stream_usage;
                    break;
                }
                StreamEvent::Error { message } => return Err(ProviderError::Other(message)),
                StreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(&delta),
                StreamEvent::ToolCallEnd { .. } | StreamEvent::MessageStart { .. } => {}
            }
        }

        Ok(ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(text),
                tool_call_id: None,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            },
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            usage,
            stop_reason: None,
        })
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        log_debug_request(&request, "ChatGPT Codex");
        let response = self.send(&Self::request_body(&request, true)).await?;
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ProviderError::RateLimited);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: body.chars().take(2_000).collect(),
            });
        }

        let (receiver, producer) = spawn_responses_stream_producer(response.bytes_stream());
        Ok(Box::new(OpenAiStream::new(receiver, producer)))
    }

    async fn list_models(&self) -> Result<Vec<ProviderModel>> {
        Ok(self
            .codex
            .list_models()
            .await
            .map_err(|error| ProviderError::Other(error.to_string()))?
            .into_iter()
            .map(|model| ProviderModel {
                id: model.id,
                owned_by: Some("chatgpt".into()),
                kind: None,
            })
            .collect())
    }

    fn context_window(&self, _model: &str) -> usize {
        272_000
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn provider_name(&self) -> &'static str {
        "codex"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_body_is_stateless_and_uses_first_party_response_fields() {
        let body = ChatGptCodexProvider::request_body(
            &ChatRequest {
                model: "gpt-5-codex".into(),
                messages: vec![],
                tools: vec![],
                temperature: None,
                system: Some("Be useful".into()),
                reasoning_effort: Some("medium".into()),
            },
            true,
        );

        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tool_choice"], "auto");
        assert!(body.get("max_output_tokens").is_none());
    }
}
