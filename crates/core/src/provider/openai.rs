use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

use super::{ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, Result, StreamEvent};
use crate::provider::types::{
    ChatMessage, ContentPart, MessageContent, Role, TokenUsage,
};

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    context_windows: Vec<(String, usize)>,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4o".to_string(),
            context_windows: vec![
                ("gpt-4o".to_string(), 128_000),
                ("gpt-4o-mini".to_string(), 128_000),
                ("gpt-4-turbo".to_string(), 128_000),
            ],
        }
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.to_string();
        self
    }

    pub fn with_default_model(mut self, model: &str) -> Self {
        self.default_model = model.to_string();
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

fn role_system_message(msg: &ChatMessage) -> Value {
    match &msg.content {
        MessageContent::Text(text) => json!({
            "role": "system",
            "content": text,
        }),
        MessageContent::Parts(parts) => {
            let content: Vec<Value> = parts.iter().map(convert_content_part).collect();
            json!({
                "role": "system",
                "content": content,
            })
        }
    }
}

fn role_user_message(msg: &ChatMessage) -> Value {
    match &msg.content {
        MessageContent::Text(text) => json!({
            "role": "user",
            "content": text,
        }),
        MessageContent::Parts(parts) => {
            let content: Vec<Value> = parts.iter().map(convert_content_part).collect();
            json!({
                "role": "user",
                "content": content,
            })
        }
    }
}

fn role_assistant_message(msg: &ChatMessage) -> Value {
    let mut obj = json!({ "role": "assistant" });

    match &msg.content {
        MessageContent::Text(text) => {
            obj["content"] = json!(text);
        }
        MessageContent::Parts(parts) => {
            let content: Vec<Value> = parts.iter().map(convert_content_part).collect();
            obj["content"] = Value::Array(content);
        }
    }

    if let Some(tool_calls) = &msg.tool_calls {
        obj["tool_calls"] = Value::Array(
            tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }
                    })
                })
                .collect(),
        );
    }

    obj
}

fn role_tool_message(msg: &ChatMessage) -> Value {
    let mut obj = json!({ "role": "tool" });

    match &msg.content {
        MessageContent::Text(text) => {
            obj["content"] = json!(text);
        }
        MessageContent::Parts(parts) => {
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            obj["content"] = json!(text);
        }
    }

    if let Some(tool_call_id) = &msg.tool_call_id {
        obj["tool_call_id"] = json!(tool_call_id);
    }

    obj
}

fn convert_content_part(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({
            "type": "text",
            "text": text,
        }),
        ContentPart::Image { source } => {
            let data_url = format!(
                "data:{};base64,{}",
                source.media_type, source.data
            );
            json!({
                "type": "image_url",
                "image_url": {
                    "url": data_url,
                }
            })
        }
        ContentPart::ToolUse { id, name, input } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentPart::ToolResult { tool_use_id, content } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }),
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
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
                    let content_parts: Vec<ContentPart> = parts
                        .iter()
                        .map(|p| parse_content_part(p))
                        .collect();
                    MessageContent::Parts(content_parts)
                }
                None => MessageContent::Text(String::new()),
            },
        };

        let tool_calls = message_json["tool_calls"].as_array().map(|tc_array| {
            tc_array
                .iter()
                .map(|tc| {
                    crate::provider::types::ToolCall {
                        id: tc["id"].as_str().unwrap_or("").to_string(),
                        call_type: tc["type"].as_str().unwrap_or("function").to_string(),
                        function: crate::provider::types::FunctionCall {
                            name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                            arguments: tc["function"]["arguments"].as_str().unwrap_or("{}").to_string(),
                        },
                    }
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
            cache_creation_input_tokens: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()),
        });

        let stop_reason = choice["finish_reason"].as_str().map(|s| s.to_string());

        Ok(ChatResponse {
            message,
            usage,
            stop_reason,
        })
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
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

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut byte_stream = response.bytes_stream();

        tokio::spawn(async move {
            let mut buffer = String::new();
            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    let _ = tx.send(Ok(StreamEvent::MessageEnd { usage: None }));
                                    continue;
                                }
                                match serde_json::from_str::<Value>(data) {
                                    Ok(json) => {
                                        if let Some(choices) = json["choices"].as_array() {
                                            for choice in choices {
                                                let delta = &choice["delta"];
                                                if let Some(content) = delta["content"].as_str() {
                                                    if !content.is_empty() {
                                                        let _ = tx.send(Ok(StreamEvent::TextDelta {
                                                            text: content.to_string(),
                                                        }));
                                                    }
                                                }
                                                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                                                    for tc in tool_calls {
                                                        let id = tc["id"].as_str().unwrap_or("");
                                                        let name = tc["function"]["name"].as_str().unwrap_or("");
                                                        let args = tc["function"]["arguments"].as_str().unwrap_or("");

                                                        if !name.is_empty() && tc["index"].is_u64() {
                                                            let _ = tx.send(Ok(StreamEvent::ToolCallDelta {
                                                                id: id.to_string(),
                                                                name: name.to_string(),
                                                                arguments_delta: args.to_string(),
                                                            }));
                                                        } else if !args.is_empty() {
                                                            let _ = tx.send(Ok(StreamEvent::ToolCallDelta {
                                                                id: id.to_string(),
                                                                name: name.to_string(),
                                                                arguments_delta: args.to_string(),
                                                            }));
                                                        }
                                                    }
                                                }
                                            }
                                            if let Some(choice) = choices.first() {
                                                let fr = choice["finish_reason"].as_str().unwrap_or("");
                                                if !fr.is_empty() && fr != "null" {
                                                    let usage = json.get("usage").map(|u| TokenUsage {
                                                        input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                                                        output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                                                        cache_read_input_tokens: u
                                                            .get("cache_read_input_tokens")
                                                            .and_then(|v| v.as_u64()),
                                                        cache_creation_input_tokens: u
                                                            .get("cache_creation_input_tokens")
                                                            .and_then(|v| v.as_u64()),
                                                    });
                                                    let _ = tx.send(Ok(StreamEvent::MessageEnd { usage }));
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Err(ProviderError::Serde(e)));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ProviderError::Http(e)));
                        break;
                    }
                }
            }
        });

        use futures::stream::poll_fn;
        use std::task::Poll;

        let mut rx = rx;
        let stream = poll_fn(move |cx| match rx.poll_recv(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        });
        Ok(Box::new(stream))
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

fn parse_content_part(value: &Value) -> ContentPart {
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

    #[test]
    fn test_context_window() {
        let provider = OpenAiProvider::new("test-key".into());
        assert_eq!(provider.context_window("gpt-4o"), 128_000);
        assert_eq!(provider.context_window("gpt-4o-mini"), 128_000);
        assert_eq!(provider.context_window("gpt-4-turbo"), 128_000);
        assert_eq!(provider.context_window("unknown-model"), 128_000);
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
    }
}
