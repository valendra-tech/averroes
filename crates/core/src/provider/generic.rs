use async_trait::async_trait;
use serde_json::{json, Value};

use super::openai::{
    parse_content_part, role_assistant_message, role_system_message, role_tool_message,
    role_user_message, spawn_openai_stream_producer, OpenAiStream,
};
use super::{
    log_debug_request, ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, Result,
};
use crate::provider::types::*;

pub struct GenericProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    context_windows: Vec<(String, usize)>,
}

impl GenericProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
            default_model: "gpt-4o".to_string(),
            context_windows: vec![("gpt-4o".to_string(), 128_000)],
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

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "stream": stream,
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
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
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
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
            usage,
            stop_reason,
        })
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        log_debug_request(&request, "Generic");
        let body = self.build_body(&request, true);
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
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
            max_tokens: None,
            temperature: None,
            system: None,
        };

        let body = provider.build_body(&request, false);
        assert_eq!(body["model"], "gpt-4o");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "hello");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], false);

        let stream_body = provider.build_body(&request, true);
        assert_eq!(stream_body["stream_options"]["include_usage"], true);
    }
}
