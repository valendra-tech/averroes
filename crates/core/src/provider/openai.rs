use async_trait::async_trait;
use serde_json::Value;
mod request;
mod stream;

pub(crate) use request::{
    role_assistant_message, role_system_message, role_tool_message, role_user_message,
};
pub(crate) use stream::{
    spawn_openai_stream_producer, spawn_responses_stream_producer, OpenAiStream,
};

use super::{
    log_debug_request, model_uses_responses_api, parse_openai_embeddings, parse_provider_models,
    ChatRequest, ChatResponse, ChatStream, EmbeddingRequest, EmbeddingResponse, Provider,
    ProviderError, ProviderModel, Result, StreamEvent,
};
use crate::connection::ConnectionKind;
use crate::provider::hooks::{ModelDiscovery, ProviderRegistry, StandardProviderHook};
use crate::provider::reasoning::{chat_reasoning, responses_reasoning};
use crate::provider::types::{
    ChatMessage, ContentPart, FunctionCall, MessageContent, Role, TokenUsage, ToolCall,
};

pub(crate) fn register_provider_hook(registry: &mut ProviderRegistry) {
    registry.register(StandardProviderHook::new(
        "openai",
        ConnectionKind::OpenAi,
        Some("openai"),
        ModelDiscovery::RemoteApi,
    ));
}

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    headers: Vec<(String, String)>,
    default_model: String,
    responses_api: Option<bool>,
    context_windows: Vec<(String, usize)>,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
            headers: Vec::new(),
            default_model: "gpt-4o".to_string(),
            responses_api: None,
            context_windows: vec![
                ("gpt-5.6-sol".to_string(), 1_050_000),
                ("gpt-5.6-terra".to_string(), 1_050_000),
                ("gpt-5.6-luna".to_string(), 1_050_000),
                ("gpt-4o".to_string(), 128_000),
                ("gpt-4o-mini".to_string(), 128_000),
                ("gpt-4-turbo".to_string(), 128_000),
            ],
        }
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.trim().trim_end_matches('/').to_string();
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        self.headers.iter().fold(
            request.header("Authorization", format!("Bearer {}", self.api_key)),
            |request, (name, value)| request.header(name, value),
        )
    }

    pub fn with_default_model(mut self, model: &str) -> Self {
        self.default_model = model.to_string();
        self
    }

    /// Uses the route explicitly advertised by a provider catalog. When left
    /// unset, OpenAI's model-name heuristic preserves the existing behaviour.
    pub fn with_responses_api(mut self, enabled: bool) -> Self {
        self.responses_api = Some(enabled);
        self
    }

    fn uses_responses_api(&self, model: &str) -> bool {
        self.responses_api
            .unwrap_or_else(|| model_uses_responses_api(model))
    }

    fn should_use_responses_api(&self, request: &ChatRequest) -> bool {
        self.uses_responses_api(&request.model)
            || (self.responses_api.is_none() && request_has_tool_images(request))
    }

    pub fn build_body(&self, request: &ChatRequest, stream: bool) -> Value {
        request::build_chat_body(request, stream)
    }

    /// Builds a standard OpenAI Responses request. Kept public within the
    /// provider module because the first-party Codex transport shares this
    /// schema.
    pub(crate) fn build_responses_request(request: &ChatRequest, stream: bool) -> Value {
        request::build_responses_body(request, stream)
    }

    fn parse_responses_output(value: &Value) -> (String, Option<Vec<ToolCall>>) {
        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(output) = value.get("output").and_then(Value::as_array) {
            for item in output {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    tool_calls.push(ToolCall {
                        id: item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            arguments: item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}")
                                .to_string(),
                        },
                    });
                    continue;
                }
                if item.get("type").and_then(Value::as_str) != Some("message") {
                    continue;
                }
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                if let Some(t) = part.get("text").and_then(Value::as_str) {
                                    text.push_str(t);
                                }
                            }
                            Some("function_call") => {
                                tool_calls.push(ToolCall {
                                    id: part
                                        .get("call_id")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                    call_type: "function".into(),
                                    function: FunctionCall {
                                        name: part
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string(),
                                        arguments: part
                                            .get("arguments")
                                            .and_then(Value::as_str)
                                            .unwrap_or("{}")
                                            .to_string(),
                                    },
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        (
            text,
            if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        )
    }

    fn parse_responses_usage(value: &Value) -> Option<TokenUsage> {
        value.get("usage").map(|u| TokenUsage {
            input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
            cache_read_input_tokens: u
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64),
            cache_creation_input_tokens: None,
            reasoning_output_tokens: u
                .get("output_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(Value::as_u64),
        })
    }

    async fn responses_chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        log_debug_request(&request, "OpenAI Responses");
        let body = Self::build_responses_request(&request, false);
        let response = self
            .authenticated(self.client.post(format!("{}/responses", self.base_url)))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: error_body,
            });
        }

        let raw: Value = response.json().await.map_err(ProviderError::from)?;

        let (text, tool_calls) = Self::parse_responses_output(&raw);
        let reasoning = responses_reasoning(&raw);
        let usage = Self::parse_responses_usage(&raw);

        Ok(ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(text),
                tool_call_id: None,
                tool_calls,
            },
            reasoning,
            usage,
            stop_reason: None,
        })
    }

    async fn responses_chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        log_debug_request(&request, "OpenAI Responses");
        let body = Self::build_responses_request(&request, true);
        let response = self
            .authenticated(self.client.post(format!("{}/responses", self.base_url)))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: error_body,
            });
        }

        let (receiver, producer) = spawn_responses_stream_producer(response.bytes_stream());
        Ok(Box::new(OpenAiStream::new(receiver, producer)))
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        if self.should_use_responses_api(&request) {
            return self.responses_chat(request).await;
        }

        log_debug_request(&request, "OpenAI");
        let body = self.build_body(&request, false);
        let response = self
            .authenticated(
                self.client
                    .post(format!("{}/chat/completions", self.base_url)),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: error_body,
            });
        }

        let raw: Value = response.json().await.map_err(ProviderError::from)?;

        let choices = raw["choices"]
            .as_array()
            .ok_or_else(|| ProviderError::Other("no choices in response".into()))?;
        let choice = choices
            .first()
            .ok_or_else(|| ProviderError::Other("empty choices array".into()))?;

        let message_json = &choice["message"];

        let content = match message_json["content"].as_str() {
            Some(text) => MessageContent::Text(text.to_string()),
            None => match message_json["content"].as_array() {
                Some(parts) => {
                    let content_parts: Vec<ContentPart> =
                        parts.iter().map(|p| parse_content_part(p)).collect();
                    MessageContent::Parts(content_parts)
                }
                None => MessageContent::Text(String::new()),
            },
        };

        let tool_calls = message_json["tool_calls"].as_array().map(|tc_array| {
            tc_array
                .iter()
                .map(|tc| crate::provider::types::ToolCall {
                    id: tc["id"].as_str().unwrap_or("").to_string(),
                    call_type: tc["type"].as_str().unwrap_or("function").to_string(),
                    function: crate::provider::types::FunctionCall {
                        name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                        arguments: tc["function"]["arguments"]
                            .as_str()
                            .unwrap_or("{}")
                            .to_string(),
                    },
                })
                .collect()
        });

        let message = ChatMessage {
            role: Role::Assistant,
            content,
            tool_call_id: None,
            tool_calls,
        };
        let reasoning = chat_reasoning(message_json);

        let usage = raw.get("usage").map(|u| TokenUsage {
            input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            cache_read_input_tokens: u
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| u.get("cache_read_input_tokens").and_then(Value::as_u64)),
            cache_creation_input_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64()),
            reasoning_output_tokens: u
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(Value::as_u64),
        });

        let stop_reason = choice["finish_reason"].as_str().map(|s| s.to_string());

        Ok(ChatResponse {
            message,
            reasoning,
            usage,
            stop_reason,
        })
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        if self.should_use_responses_api(&request) {
            return self.responses_chat_stream(request).await;
        }

        log_debug_request(&request, "OpenAI");
        let body = self.build_body(&request, true);
        let response = self
            .authenticated(
                self.client
                    .post(format!("{}/chat/completions", self.base_url)),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                body: error_body,
            });
        }

        let (receiver, producer) = spawn_openai_stream_producer(response.bytes_stream());
        Ok(Box::new(OpenAiStream::new(receiver, producer)))
    }

    async fn list_models(&self) -> Result<Vec<ProviderModel>> {
        let response = self
            .authenticated(self.client.get(format!("{}/models", self.base_url)))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        Ok(parse_provider_models(&response.json().await?))
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let response = self
            .authenticated(self.client.post(format!("{}/embeddings", self.base_url)))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": request.model,
                "input": request.input,
            }))
            .send()
            .await?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(ProviderError::Api {
                status,
                body: response.text().await.unwrap_or_default(),
            });
        }
        parse_openai_embeddings(&response.json().await?)
    }

    fn context_window(&self, model: &str) -> usize {
        self.context_windows
            .iter()
            .find(|(m, _)| m == model)
            .map(|(_, w)| *w)
            .or_else(|| (model == "gpt-5.6" || model.starts_with("gpt-5.6-")).then_some(1_050_000))
            .unwrap_or(128_000)
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

fn request_has_tool_images(request: &ChatRequest) -> bool {
    request.messages.iter().any(|message| {
        message.role == Role::Tool
            && matches!(
                &message.content,
                MessageContent::Parts(parts)
                    if parts.iter().any(|part| matches!(part, ContentPart::Image { .. }))
            )
    })
}

pub(crate) fn parse_content_part(value: &Value) -> ContentPart {
    match value["type"].as_str() {
        Some("text") => ContentPart::Text {
            text: value["text"].as_str().unwrap_or("").to_string(),
        },
        Some("image_url") => ContentPart::Image {
            source: crate::provider::types::ImageSource {
                media_type: "image/png".to_string(),
                data: String::new(),
            },
        },
        Some("tool_use") => ContentPart::ToolUse {
            id: value["id"].as_str().unwrap_or("").to_string(),
            name: value["name"].as_str().unwrap_or("").to_string(),
            input: value["input"].clone(),
        },
        Some("tool_result") => ContentPart::ToolResult {
            tool_use_id: value["tool_use_id"].as_str().unwrap_or("").to_string(),
            content: value["content"].as_str().unwrap_or("").to_string(),
        },
        _ => ContentPart::Text {
            text: value.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::*;
    use bytes::Bytes;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct ProducerDropSignal(Arc<AtomicBool>);

    impl Drop for ProducerDropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_context_window() {
        let provider = OpenAiProvider::new("test-key".into());
        assert_eq!(provider.context_window("gpt-5.6-sol"), 1_050_000);
        assert_eq!(provider.context_window("gpt-5.6-terra"), 1_050_000);
        assert_eq!(provider.context_window("gpt-5.6-luna"), 1_050_000);
        assert_eq!(
            provider.context_window("gpt-5.6-luna-2026-08-01"),
            1_050_000
        );
        assert_eq!(provider.context_window("gpt-4o"), 128_000);
        assert_eq!(provider.context_window("gpt-4o-mini"), 128_000);
        assert_eq!(provider.context_window("gpt-4-turbo"), 128_000);
        assert_eq!(provider.context_window("unknown-model"), 128_000);
    }

    #[test]
    fn responses_usage_preserves_cache_and_reasoning_breakdowns() {
        let response = serde_json::json!({
            "usage": {
                "input_tokens": 50_000,
                "output_tokens": 2_000,
                "input_tokens_details": { "cached_tokens": 48_000 },
                "output_tokens_details": { "reasoning_tokens": 1_500 }
            }
        });

        let usage = OpenAiProvider::parse_responses_usage(&response).unwrap();

        assert_eq!(usage.input_tokens, 50_000);
        assert_eq!(usage.cache_read_input_tokens, Some(48_000));
        assert_eq!(usage.output_tokens, 2_000);
        assert_eq!(usage.reasoning_output_tokens, Some(1_500));
    }

    #[test]
    fn test_build_body_basic() {
        let provider = OpenAiProvider::new("test-key".into());
        let request = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("hello".into()),
                tool_call_id: None,
                tool_calls: None,
            }],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        let body = provider.build_body(&request, false);
        assert_eq!(body["model"], "gpt-4o");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "hello");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["stream"], false);

        let stream_body = provider.build_body(&request, true);
        assert_eq!(stream_body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn reasoning_models_omit_temperature() {
        let provider = OpenAiProvider::new("test-key".into());
        let request = ChatRequest {
            model: "o3".into(),
            messages: vec![],
            tools: vec![],
            temperature: Some(0.7),
            system: None,
            reasoning_effort: None,
        };

        let body = provider.build_body(&request, false);

        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("max_tokens").is_none());
        assert!(body["temperature"].is_null());
    }

    #[test]
    fn requests_do_not_include_output_limits() {
        let provider = OpenAiProvider::new("test-key".into());
        let request = ChatRequest {
            model: "gpt-5.6-luna".into(),
            messages: vec![],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        let chat = provider.build_body(&request, true);
        assert!(chat.get("max_tokens").is_none());
        assert!(chat.get("max_completion_tokens").is_none());
        assert_eq!(chat["stream_options"]["include_usage"], true);

        let responses = OpenAiProvider::build_responses_request(&request, true);
        assert!(responses.get("max_output_tokens").is_none());
    }

    #[test]
    fn responses_tool_output_matches_the_assistant_call_id() {
        let request = ChatRequest {
            model: "gpt-5.6-luna".into(),
            messages: vec![
                ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text(String::new()),
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call-123".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "call_agents".into(),
                            arguments: r#"{"prompt":"2+2"}"#.into(),
                        },
                    }]),
                },
                ChatMessage {
                    role: Role::Tool,
                    content: MessageContent::Text("4".into()),
                    tool_call_id: Some("call-123".into()),
                    tool_calls: None,
                },
            ],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        let input = OpenAiProvider::build_responses_request(&request, false)["input"]
            .as_array()
            .unwrap()
            .to_vec();
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "call-123");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], "call-123");
    }

    #[test]
    fn multimodal_tool_outputs_reach_both_openai_apis() {
        let message = ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Screenshot captured".into(),
                },
                ContentPart::Image {
                    source: ImageSource {
                        media_type: "image/png".into(),
                        data: "aW1hZ2U=".into(),
                    },
                },
            ]),
            tool_call_id: Some("call-image".into()),
            tool_calls: None,
        };
        let request = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![message],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        assert!(OpenAiProvider::new("test-key".into()).should_use_responses_api(&request));
        assert!(!OpenAiProvider::new("test-key".into())
            .with_responses_api(false)
            .should_use_responses_api(&request));

        let chat = OpenAiProvider::new("test-key".into()).build_body(&request, false);
        let chat_content = chat["messages"][0]["content"].as_array().unwrap();
        assert_eq!(chat_content[0]["type"], "text");
        assert_eq!(chat_content[1]["type"], "image_url");
        assert_eq!(
            chat_content[1]["image_url"]["url"],
            "data:image/png;base64,aW1hZ2U="
        );

        let responses = OpenAiProvider::build_responses_request(&request, false);
        let output = responses["input"][0]["output"].as_array().unwrap();
        assert_eq!(responses["input"][0]["type"], "function_call_output");
        assert_eq!(responses["input"][0]["call_id"], "call-image");
        assert_eq!(output[0]["type"], "input_text");
        assert_eq!(output[1]["type"], "input_image");
        assert_eq!(output[1]["image_url"], "data:image/png;base64,aW1hZ2U=");
    }

    #[test]
    fn responses_parser_accepts_top_level_function_call_items() {
        let (text, calls) = OpenAiProvider::parse_responses_output(&serde_json::json!({
            "output": [
                {"type":"function_call","call_id":"call-7","name":"call_agents","arguments":r#"{"prompt":"2+2"}"#}
            ]
        }));
        assert!(text.is_empty());
        let call = calls.unwrap().pop().unwrap();
        assert_eq!(call.id, "call-7");
        assert_eq!(call.function.name, "call_agents");
    }

    #[test]
    fn assistant_tool_use_parts_become_tool_calls_not_content_blocks() {
        let provider = OpenAiProvider::new("test-key".into());
        let request = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "I will inspect that.".into(),
                    },
                    ContentPart::ToolUse {
                        id: "tool-1".into(),
                        name: "file_read".into(),
                        input: serde_json::json!({"path": "README.md"}),
                    },
                ]),
                tool_call_id: None,
                tool_calls: None,
            }],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        let body = provider.build_body(&request, false);
        let message = &body["messages"][0];
        let content = message["content"].as_array().expect("content blocks");

        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(message["tool_calls"][0]["id"], "tool-1");
    }

    #[test]
    fn assistant_tool_calls_are_deduplicated() {
        let provider = OpenAiProvider::new("test-key".into());
        let tool_call = ToolCall {
            id: "tool-1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "file_read".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
        };
        let request = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("I will inspect that.".into()),
                tool_call_id: None,
                tool_calls: Some(vec![tool_call.clone(), tool_call]),
            }],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        let body = provider.build_body(&request, false);

        assert_eq!(
            body["messages"][0]["tool_calls"].as_array().unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn dropping_stream_aborts_producer_task() {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let producer_dropped = Arc::new(AtomicBool::new(false));
        let signal = ProducerDropSignal(producer_dropped.clone());
        let producer = tokio::spawn(async move {
            let _signal = signal;
            std::future::pending::<()>().await;
        });

        let stream = OpenAiStream::new(rx, producer);
        drop(stream);
        tokio::task::yield_now().await;

        assert!(producer_dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn done_marker_emits_one_terminal_event_and_stops_producer() {
        let byte_stream = futures::stream::iter(vec![
            Ok(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"before\"},\"finish_reason\":null}]}\n",
            )),
            Ok(Bytes::from("data: [DONE]\n")),
            Ok(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"after\"},\"finish_reason\":null}]}\n",
            )),
        ]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let mut stream = OpenAiStream::new(receiver, producer);

        let events = stream.by_ref().collect::<Vec<_>>().await;
        let terminal_events = events
            .iter()
            .filter(|event| matches!(event, Ok(StreamEvent::MessageEnd { .. })))
            .count();

        assert_eq!(terminal_events, 1);
        assert!(!events.iter().any(|event| {
            matches!(event, Ok(StreamEvent::TextDelta { text }) if text == "after")
        }));
    }

    #[tokio::test]
    async fn finish_reason_and_done_emit_one_terminal_event() {
        let byte_stream = futures::stream::iter(vec![
            Ok(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"before\"},\"finish_reason\":\"stop\"}]}\n",
            )),
            Ok(Bytes::from("data: [DONE]\n")),
        ]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Ok(StreamEvent::MessageEnd { .. })))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn duplicate_reasoning_aliases_are_deduplicated_per_stream_frame() {
        let frame = concat!(
            r#"data: {"choices":[{"delta":{"reasoning_content":"ha","reasoning":"ha"},"finish_reason":null}]}"#,
            "\n"
        );
        let byte_stream = futures::stream::iter(vec![
            Ok(Bytes::from(frame)),
            Ok(Bytes::from(frame)),
            Ok(Bytes::from("data: [DONE]\n")),
        ]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;
        let reasoning = events
            .into_iter()
            .filter_map(|event| match event.ok()? {
                StreamEvent::ReasoningDelta { text } => Some(text),
                _ => None,
            })
            .collect::<String>();

        // Alias duplication inside each QDivZero frame disappears, while the
        // model's legitimate repetition across independent frames remains.
        assert_eq!(reasoning, "haha");
    }

    #[tokio::test]
    async fn streamed_tool_call_continuations_keep_identity_and_close() {
        let byte_stream = futures::stream::iter(vec![
            Ok(Bytes::from(concat!(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"file_read","arguments":"part-1"}}]},"finish_reason":null}]}"#,
                "\n"
            ))),
            Ok(Bytes::from(concat!(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"part-2"}}]},"finish_reason":null}]}"#,
                "\n"
            ))),
            Ok(Bytes::from(concat!(
                r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                "\n"
            ))),
            Ok(Bytes::from(concat!(
                r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
                "\n"
            ))),
        ]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &events[0],
            Ok(StreamEvent::ToolCallDelta { id, name, arguments_delta })
                if id == "call-1"
                    && name == "file_read"
                    && arguments_delta == "part-1"
        ));
        assert!(matches!(
            &events[1],
            Ok(StreamEvent::ToolCallDelta { id, name, arguments_delta })
                if id == "call-1"
                    && name == "file_read"
                    && arguments_delta == "part-2"
        ));
        assert!(matches!(
            &events[2],
            Ok(StreamEvent::ToolCallEnd { id }) if id == "call-1"
        ));
        assert!(matches!(
            &events[3],
            Ok(StreamEvent::MessageEnd {
                usage: Some(TokenUsage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..
                })
            })
        ));
    }

    #[tokio::test]
    async fn error_frame_emits_stream_error() {
        let byte_stream = futures::stream::iter(vec![Ok(Bytes::from(
            "data: {\"error\":{\"message\":\"quota exceeded\"}}\n",
        ))]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.as_slice(),
            [Ok(StreamEvent::Error { message })] if message == "quota exceeded"
        ));
    }

    #[tokio::test]
    async fn split_utf8_bytes_are_preserved_in_stream_text() {
        let mut first = br##"data: {"choices":[{"delta":{"content":"caf"##.to_vec();
        first.push(0xc3);
        let mut second = vec![0xa9];
        second.extend_from_slice(br#""},"finish_reason":null}]}"#);
        second.push(b'\n');
        let byte_stream =
            futures::stream::iter(vec![Ok(Bytes::from(first)), Ok(Bytes::from(second))]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.as_slice(),
            [Ok(StreamEvent::TextDelta { text })] if text == "café"
        ));
    }

    #[tokio::test]
    async fn usage_only_chunk_ends_stream_with_actual_usage() {
        let byte_stream = futures::stream::iter(vec![Ok(Bytes::from(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":5}}\n",
        ))]);
        let (receiver, producer) = spawn_openai_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.as_slice(),
            [Ok(StreamEvent::MessageEnd {
                usage: Some(TokenUsage {
                    input_tokens: 11,
                    output_tokens: 5,
                    ..
                })
            })]
        ));
    }

    #[tokio::test]
    async fn responses_stream_preserves_call_id_for_tool_results() {
        let byte_stream = futures::stream::iter(vec![
            Ok(Bytes::from(concat!(
                r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"item-1","call_id":"call-1","name":"file_read","arguments":""}}"#,
                "\n"
            ))),
            Ok(Bytes::from(concat!(
                r#"data: {"type":"response.function_call_arguments.delta","item_id":"item-1","delta":"{\"path\":\"README.md\"}"}"#,
                "\n"
            ))),
            Ok(Bytes::from(concat!(
                r#"data: {"type":"response.output_item.done","item":{"type":"function_call","id":"item-1","call_id":"call-1"}}"#,
                "\n"
            ))),
            Ok(Bytes::from(concat!(
                r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":2}}}"#,
                "\n"
            ))),
        ]);
        let (receiver, producer) = spawn_responses_stream_producer(byte_stream);
        let events = OpenAiStream::new(receiver, producer)
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &events[0],
            Ok(StreamEvent::ToolCallDelta { id, name, arguments_delta })
                if id == "call-1"
                    && name == "file_read"
                    && arguments_delta == r#"{"path":"README.md"}"#
        ));
        assert!(matches!(
            &events[1],
            Ok(StreamEvent::ToolCallEnd { id }) if id == "call-1"
        ));
        assert!(matches!(
            &events[2],
            Ok(StreamEvent::MessageEnd {
                usage: Some(TokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..
                })
            })
        ));
    }
}
