use async_trait::async_trait;
use serde_json::{json, Value};

use super::openai::{
    parse_content_part, role_assistant_message, role_system_message, role_tool_message,
    role_user_message, spawn_openai_stream_producer, OpenAiStream,
};
use super::qdivzero::parse_serving_endpoints;
use super::{
    log_debug_request, model_uses_reasoning_api, parse_openai_embeddings, parse_provider_models,
    ChatRequest, ChatResponse, ChatStream, EmbeddingRequest, EmbeddingResponse, Provider,
    ProviderError, ProviderModel, Result,
};
use crate::connection::ConnectionKind;
use crate::provider::hooks::{ModelDiscovery, ProviderRegistry, StandardProviderHook};
use crate::provider::reasoning::chat_reasoning;
use crate::provider::types::*;

pub(crate) fn register_provider_hooks(registry: &mut ProviderRegistry) {
    registry.register(StandardProviderHook::new(
        "deepseek",
        ConnectionKind::DeepSeek,
        Some("deepseek"),
        ModelDiscovery::RemoteApi,
    ));
    registry.register(StandardProviderHook::new(
        "groq",
        ConnectionKind::Groq,
        Some("groq"),
        ModelDiscovery::RemoteApi,
    ));
    registry.register(StandardProviderHook::new(
        "ollama",
        ConnectionKind::Ollama,
        Some("ollama"),
        ModelDiscovery::RemoteApi,
    ));
    registry.register(StandardProviderHook::new(
        "ollama-cloud",
        ConnectionKind::OllamaCloud,
        Some("ollama-cloud"),
        ModelDiscovery::RemoteApi,
    ));
    registry.register(StandardProviderHook::new(
        "compatible",
        ConnectionKind::Compatible,
        Some("openai"),
        ModelDiscovery::RemoteApi,
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelListing {
    OpenAi,
    OllamaTags,
    QDivZero,
}

pub struct GenericProvider {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    models_url: String,
    embedding_url: String,
    model_listing: ModelListing,
    default_model: String,
    context_windows: Vec<(String, usize)>,
}

impl GenericProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self::with_auth(Some(api_key), base_url)
    }

    pub fn without_auth(base_url: String) -> Self {
        Self::with_auth(None, base_url)
    }

    pub fn ollama(base_url: &str) -> Self {
        let root = base_url
            .trim()
            .trim_end_matches('/')
            .strip_suffix("/v1")
            .unwrap_or_else(|| base_url.trim().trim_end_matches('/'));
        let chat_base = format!("{root}/v1");
        Self::without_auth(chat_base)
            .with_model_listing(format!("{root}/api/tags"), ModelListing::OllamaTags)
            .with_embedding_url(format!("{root}/api/embed"))
    }

    pub fn qdivzero(api_key: String, base_url: &str) -> Self {
        let root = base_url
            .trim()
            .trim_end_matches('/')
            .strip_suffix("/v1")
            .unwrap_or_else(|| base_url.trim().trim_end_matches('/'));
        Self::with_auth(Some(api_key), format!("{root}/v1"))
            .with_model_listing(format!("{root}/serving-endpoints"), ModelListing::QDivZero)
    }

    fn with_auth(api_key: Option<String>, base_url: String) -> Self {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.and_then(normalize_api_key),
            models_url: format!("{base_url}/models"),
            embedding_url: format!("{base_url}/embeddings"),
            base_url,
            model_listing: ModelListing::OpenAi,
            default_model: "gpt-4o".to_string(),
            context_windows: vec![("gpt-4o".to_string(), 128_000)],
        }
    }

    fn with_model_listing(mut self, models_url: String, model_listing: ModelListing) -> Self {
        self.models_url = models_url.trim().trim_end_matches('/').to_string();
        self.model_listing = model_listing;
        self
    }

    fn with_embedding_url(mut self, embedding_url: String) -> Self {
        self.embedding_url = embedding_url.trim().trim_end_matches('/').to_string();
        self
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.api_key.as_deref() {
            Some(api_key) => request.header("Authorization", format!("Bearer {api_key}")),
            None => request,
        }
    }

    pub fn with_default_model(mut self, model: &str) -> Self {
        self.default_model = model.to_string();
        self
    }

    pub fn with_context_windows(mut self, windows: Vec<(String, usize)>) -> Self {
        self.context_windows = windows;
        self
    }

    pub fn build_body(&self, request: &ChatRequest, stream: bool) -> Value {
        let mut messages: Vec<Value> = Vec::new();

        if let Some(system) = &request.system {
            messages.push(json!({
                "role": "system",
                "content": system,
            }));
        }

        for msg in &request.messages {
            match msg.role {
                Role::System => {
                    messages.push(role_system_message(msg));
                }
                Role::User => {
                    messages.push(role_user_message(msg));
                }
                Role::Assistant => {
                    messages.push(role_assistant_message(msg));
                }
                Role::Tool => {
                    messages.push(role_tool_message(msg));
                }
            }
        }

        let use_reasoning_api = model_uses_reasoning_api(&request.model);
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
        });

        if let Some(temp) = request.temperature.filter(|_| !use_reasoning_api) {
            body["temperature"] = json!(temp);
        }

        if let Some(effort) = request
            .reasoning_effort
            .as_deref()
            .filter(|effort| !effort.trim().is_empty())
        {
            body["reasoning_effort"] = json!(effort);
        }

        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.input_schema,
                            }
                        })
                    })
                    .collect(),
            );
        }

        body
    }
}

#[async_trait]
impl Provider for GenericProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        log_debug_request(&request, "Generic");
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
                .map(|tc| ToolCall {
                    id: tc["id"].as_str().unwrap_or("").to_string(),
                    call_type: tc["type"].as_str().unwrap_or("function").to_string(),
                    function: FunctionCall {
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
            cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
            cache_creation_input_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64()),
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
        log_debug_request(&request, "Generic");
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
            .authenticated(self.client.get(&self.models_url))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        let response = response.json().await?;
        Ok(match self.model_listing {
            ModelListing::OpenAi => parse_provider_models(&response),
            ModelListing::OllamaTags => parse_ollama_models(&response),
            ModelListing::QDivZero => parse_serving_endpoints(&response),
        })
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let response = self
            .authenticated(self.client.post(&self.embedding_url))
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
        let value: Value = response.json().await?;
        if self.model_listing == ModelListing::OllamaTags {
            if let Some(embeddings) = value.get("embeddings").and_then(Value::as_array) {
                let embeddings = embeddings
                    .iter()
                    .map(|embedding| {
                        embedding
                            .as_array()
                            .map_or(&[][..], Vec::as_slice)
                            .iter()
                            .filter_map(Value::as_f64)
                            .map(|value| value as f32)
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                if embeddings.iter().all(|embedding| !embedding.is_empty()) {
                    return Ok(EmbeddingResponse { embeddings });
                }
            }
            if let Some(embedding) = value.get("embedding").and_then(Value::as_array) {
                let embedding = embedding
                    .iter()
                    .filter_map(Value::as_f64)
                    .map(|value| value as f32)
                    .collect::<Vec<_>>();
                if !embedding.is_empty() {
                    return Ok(EmbeddingResponse {
                        embeddings: vec![embedding],
                    });
                }
            }
            return Err(ProviderError::Other(
                "Ollama embedding response contains no vectors".into(),
            ));
        }
        parse_openai_embeddings(&value)
    }

    fn context_window(&self, model: &str) -> usize {
        self.context_windows
            .iter()
            .find(|(m, _)| m == model)
            .map(|(_, w)| *w)
            .unwrap_or(128_000)
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn provider_name(&self) -> &'static str {
        "generic"
    }
}

fn normalize_api_key(api_key: String) -> Option<String> {
    let key = api_key.trim();
    if key.is_empty() {
        return None;
    }

    // The UI asks for the token itself, but accepting the complete
    // `Authorization: Bearer …` value avoids sending `Bearer Bearer …` when
    // a user copies the value directly from a curl command.
    let normalized = key
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .and_then(|(index, _)| {
            let (scheme, value) = key.split_at(index);
            scheme
                .eq_ignore_ascii_case("bearer")
                .then_some(value.trim())
        })
        .unwrap_or(key);

    (!normalized.is_empty()).then(|| normalized.to_owned())
}

fn parse_ollama_models(value: &Value) -> Vec<ProviderModel> {
    value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model
                .get("name")
                .or_else(|| model.get("model"))
                .and_then(Value::as_str)?
                .trim();
            (!id.is_empty()).then(|| ProviderModel {
                id: id.to_owned(),
                owned_by: model
                    .get("details")
                    .and_then(|details| details.get("family"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                kind: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_window() {
        let provider = GenericProvider::new("test-key".into(), "https://api.example.com".into());
        assert_eq!(provider.context_window("gpt-4o"), 128_000);
        assert_eq!(provider.context_window("unknown-model"), 128_000);
    }

    #[test]
    fn test_build_body_basic() {
        let provider = GenericProvider::new("test-key".into(), "https://api.example.com".into());
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
        let provider = GenericProvider::new("test-key".into(), "https://api.example.com".into());
        let request = ChatRequest {
            model: "o4-mini".into(),
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
    fn forwards_reasoning_effort_for_compatible_reasoning_apis() {
        let provider = GenericProvider::new("test-key".into(), "https://api.example.com".into());
        let request = ChatRequest {
            model: "deepseek-v4-pro".into(),
            messages: vec![],
            tools: vec![],
            temperature: None,
            system: None,
            reasoning_effort: Some("high".into()),
        };

        assert_eq!(
            provider.build_body(&request, false)["reasoning_effort"],
            "high"
        );
    }

    #[test]
    fn parses_models_from_the_native_ollama_catalog() {
        let models = parse_ollama_models(&json!({
            "models": [{
                "name": "qwen3:8b",
                "details": { "family": "qwen3" }
            }]
        }));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "qwen3:8b");
        assert_eq!(models[0].owned_by.as_deref(), Some("qwen3"));
    }

    #[test]
    fn ollama_normalizes_a_v1_base_url_and_needs_no_api_key() {
        let provider = GenericProvider::ollama("http://localhost:11434/v1/");

        assert_eq!(provider.base_url, "http://localhost:11434/v1");
        assert_eq!(provider.models_url, "http://localhost:11434/api/tags");
        assert!(provider.api_key.is_none());
    }

    #[test]
    fn qdivzero_uses_the_openai_chat_and_embedding_routes_and_private_catalog() {
        let provider = GenericProvider::qdivzero("test-key".into(), "https://api.qdiv0.com/v1/");

        assert_eq!(provider.base_url, "https://api.qdiv0.com/v1");
        assert_eq!(
            provider.models_url,
            "https://api.qdiv0.com/serving-endpoints"
        );
        assert_eq!(
            provider.embedding_url,
            "https://api.qdiv0.com/v1/embeddings"
        );
        assert!(provider.api_key.is_some());
        assert_eq!(provider.model_listing, ModelListing::QDivZero);
    }

    #[test]
    fn bearer_prefix_is_not_sent_twice_when_a_token_is_pasted_from_curl() {
        let provider = GenericProvider::qdivzero("Bearer test-key".into(), "https://api.qdiv0.com");

        assert_eq!(provider.api_key.as_deref(), Some("test-key"));
    }
}
