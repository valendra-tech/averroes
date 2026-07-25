use async_trait::async_trait;
use serde_json::{json, Value};

use crate::provider::types::*;
use crate::provider::{ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, Result, StreamEvent, ToolDefinition};

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    context_windows: Vec<(String, usize)>,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.anthropic.com/v1".to_string(),
            default_model: "claude-sonnet-4-20250514".to_string(),
            context_windows: vec![
                ("claude-sonnet-4-20250514".to_string(), 200_000),
                ("claude-opus-4-20250514".to_string(), 200_000),
                ("claude-3-5-sonnet-20241022".to_string(), 200_000),
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

    fn build_body(&self, request: &ChatRequest, stream: bool) -> Value {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        if let Some(ref temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        if stream {
            body["stream"] = json!(true);
        }

        for msg in &request.messages {
            if msg.role == Role::System {
                if let MessageContent::Text(ref text) = msg.content {
                    body["system"] = json!(text);
                } else if let MessageContent::Parts(ref parts) = msg.content {
                    let system_text: String = parts
                        .iter()
                        .filter_map(|p| {
                            if let ContentPart::Text { text } = p {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !system_text.is_empty() {
                        body["system"] = json!(system_text);
                    }
                }
                break;
            }
        }

        let messages = self.convert_messages(&request.messages);
        body["messages"] = json!(messages);

        if !request.tools.is_empty() {
            body["tools"] = json!(self.convert_tools(&request.tools));
        }

        body
    }

    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let role_str = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "user",
                    Role::System => unreachable!(),
                };

                let mut msg = json!({ "role": role_str });

                match &m.content {
                    MessageContent::Text(text) => {
                        if m.role == Role::Tool {
                            msg["content"] = json!([{
                                "type": "tool_result",
                                "tool_use_id": m.tool_call_id.as_deref().unwrap_or(""),
                                "content": text,
                            }]);
                        } else {
                            msg["content"] = json!(text);
                        }
                    }
                    MessageContent::Parts(parts) => {
                        let blocks: Vec<Value> = parts
                            .iter()
                            .map(|p| match p {
                                ContentPart::Text { text } => {
                                    json!({ "type": "text", "text": text })
                                }
                                ContentPart::Image { source } => {
                                    json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": source.media_type,
                                            "data": source.data,
                                        }
                                    })
                                }
                                ContentPart::ToolUse { id, name, input } => {
                                    json!({
                                        "type": "tool_use",
                                        "id": id,
                                        "name": name,
                                        "input": input,
                                    })
                                }
                                ContentPart::ToolResult {
                                    tool_use_id,
                                    content,
                                } => {
                                    json!({
                                        "type": "tool_result",
                                        "tool_use_id": tool_use_id,
                                        "content": content,
                                    })
                                }
                            })
                            .collect();
                        msg["content"] = json!(blocks);
                    }
                }

                if let Some(ref tool_calls) = m.tool_calls {
                    if m.role == Role::Assistant {
                        let blocks: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                let input: Value = serde_json::from_str(&tc.function.arguments)
                                    .unwrap_or(Value::Object(serde_json::Map::new()));
                                json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "input": input,
                                })
                            })
                            .collect();

                        msg["content"] = if blocks.len() == 1 {
                            blocks[0].clone()
                        } else {
                            json!(blocks)
                        };
                    }
                }

                msg
            })
            .collect()
    }

    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect()
    }

    fn parse_content_blocks(content: &Value) -> (ChatMessage, Option<String>) {
        let mut parts = Vec::new();
        let mut text_content = String::new();
        let mut tool_calls = Vec::new();

        if let Some(blocks) = content.as_array() {
            for block in blocks {
                let block_type = block["type"].as_str().unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block["text"].as_str() {
                            text_content.push_str(text);
                            parts.push(ContentPart::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                    "tool_use" => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        let input = block["input"].clone();
                        tool_calls.push(crate::provider::types::ToolCall {
                            id: id.clone(),
                            call_type: "function".to_string(),
                            function: FunctionCall {
                                name: name.clone(),
                                arguments: input.to_string(),
                            },
                        });
                        parts.push(ContentPart::ToolUse { id, name, input });
                    }
                    _ => {}
                }
            }
        }

        let content = if parts.is_empty() {
            MessageContent::Text(text_content)
        } else {
            MessageContent::Parts(parts)
        };

        let message = ChatMessage {
            role: Role::Assistant,
            content,
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        };

        (message, None)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = self.build_body(&request, false);
        let url = format!("{}/messages", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await?;
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let response_body: Value = response.json().await?;

        let content = &response_body["content"];
        let (message, _) = Self::parse_content_blocks(content);

        let usage = response_body.get("usage").map(|u| TokenUsage {
            input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
            cache_read_input_tokens: u["cache_read_input_tokens"].as_u64(),
            cache_creation_input_tokens: u["cache_creation_input_tokens"].as_u64(),
        });

        let stop_reason = response_body["stop_reason"].as_str().map(String::from);

        Ok(ChatResponse {
            message,
            usage,
            stop_reason,
        })
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let body = self.build_body(&request, true);
        let url = format!("{}/messages", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await?;
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let bytes = response.bytes().await?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        let events: Vec<Result<StreamEvent>> = parse_sse_events(&text);

        Ok(Box::new(futures::stream::iter(events)))
    }

    fn context_window(&self, model: &str) -> usize {
        self.context_windows
            .iter()
            .find(|(m, _)| m == model)
            .map(|(_, cw)| *cw)
            .unwrap_or(200_000)
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }
}

fn parse_sse_events(text: &str) -> Vec<Result<StreamEvent>> {
    text.split("\n\n")
        .filter_map(|event| {
            let data_line = event
                .lines()
                .find(|line| line.starts_with("data: "))?;
            let json_str = &data_line[6..];
            let parsed: Value = serde_json::from_str(json_str).ok()?;

            let event_type = parsed["type"].as_str().unwrap_or("");

            match event_type {
                "content_block_delta" => {
                    let delta = &parsed["delta"];
                    if let Some(text) = delta["text"].as_str() {
                        Some(Ok(StreamEvent::TextDelta {
                            text: text.to_string(),
                        }))
                    } else {
                        None
                    }
                }
                "message_delta" => {
                    let usage = parsed.get("usage").map(|u| TokenUsage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        cache_read_input_tokens: u["cache_read_input_tokens"].as_u64(),
                        cache_creation_input_tokens: u["cache_creation_input_tokens"].as_u64(),
                    });
                    Some(Ok(StreamEvent::MessageEnd { usage }))
                }
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_window() {
        let provider = AnthropicProvider::new("test-key".into());
        assert_eq!(provider.context_window("claude-sonnet-4-20250514"), 200_000);
        assert_eq!(provider.context_window("unknown-model"), 200_000);
    }

    #[test]
    fn test_convert_messages_basic() {
        let provider = AnthropicProvider::new("test-key".into());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text("hello".into()),
            tool_call_id: None,
            tool_calls: None,
        }];

        let converted = provider.convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["role"].as_str(), Some("user"));
        assert_eq!(converted[0]["content"].as_str(), Some("hello"));
    }
}
