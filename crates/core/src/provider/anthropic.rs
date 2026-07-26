use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::provider::types::*;
use crate::provider::{
    log_debug_request, ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, Result,
    StreamEvent, ToolDefinition,
};

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    context_windows: Vec<(String, usize)>,
}

struct AnthropicStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Result<StreamEvent>>,
    producer: Option<tokio::task::JoinHandle<()>>,
}

impl AnthropicStream {
    fn new(
        receiver: tokio::sync::mpsc::UnboundedReceiver<Result<StreamEvent>>,
        producer: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            receiver,
            producer: Some(producer),
        }
    }
}

impl Unpin for AnthropicStream {}

impl Stream for AnthropicStream {
    type Item = Result<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

impl Drop for AnthropicStream {
    fn drop(&mut self) {
        if let Some(producer) = self.producer.take() {
            producer.abort();
        }
    }
}

fn spawn_anthropic_stream_producer<S>(
    mut byte_stream: S,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<Result<StreamEvent>>,
    tokio::task::JoinHandle<()>,
)
where
    S: Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let producer = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let mut message_start_usage = None;
        let mut current_tool = None;

        while let Some(chunk_result) = byte_stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(separator) = find_sse_separator(&buffer) {
                        let separator_len = sse_separator_len(&buffer[separator..]);
                        let event_bytes = buffer
                            .drain(..separator + separator_len)
                            .collect::<Vec<_>>();
                        let event = match String::from_utf8(event_bytes[..separator].to_vec()) {
                            Ok(event) => event,
                            Err(error) => {
                                let _ = tx.send(Err(ProviderError::Other(format!(
                                    "Invalid UTF-8 in Anthropic stream: {error}"
                                ))));
                                return;
                            }
                        };
                        if let Some(parsed) =
                            parse_sse_event(&event, &mut message_start_usage, &mut current_tool)
                        {
                            if tx.send(parsed).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(ProviderError::Http(error)));
                    return;
                }
            }
        }

        if !buffer.is_empty() {
            match String::from_utf8(buffer) {
                Ok(event) => {
                    if let Some(parsed) =
                        parse_sse_event(&event, &mut message_start_usage, &mut current_tool)
                    {
                        let _ = tx.send(parsed);
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(ProviderError::Other(format!(
                        "Invalid UTF-8 in Anthropic stream: {error}"
                    ))));
                }
            }
        }
    });

    (rx, producer)
}

fn find_sse_separator(buffer: &[u8]) -> Option<usize> {
    [
        buffer.windows(2).position(|window| window == b"\n\n"),
        buffer.windows(4).position(|window| window == b"\r\n\r\n"),
        buffer.windows(2).position(|window| window == b"\r\r"),
    ]
    .into_iter()
    .flatten()
    .min()
}

fn sse_separator_len(buffer: &[u8]) -> usize {
    if buffer.starts_with(b"\r\n\r\n") {
        4
    } else {
        2
    }
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

        let mut system_sections = Vec::new();
        if let Some(system) = request.system.as_deref().filter(|text| !text.is_empty()) {
            system_sections.push(system.to_string());
        }
        system_sections.extend(
            request
                .messages
                .iter()
                .filter(|message| message.role == Role::System)
                .filter_map(|message| match &message.content {
                    MessageContent::Text(text) if !text.is_empty() => Some(text.clone()),
                    MessageContent::Parts(parts) => {
                        let text = parts
                            .iter()
                            .filter_map(|part| match part {
                                ContentPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        (!text.is_empty()).then_some(text)
                    }
                    _ => None,
                }),
        );
        let system_text = system_sections.join("\n\n");
        if !system_text.is_empty() {
            body["system"] = json!(system_text);
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
                            .filter(|part| match part {
                                ContentPart::ToolUse { id, .. } if m.role == Role::Assistant => {
                                    !m.tool_calls.as_ref().is_some_and(|tool_calls| {
                                        tool_calls
                                            .iter()
                                            .any(|tool_call| tool_call.id.as_str() == id.as_str())
                                    })
                                }
                                _ => true,
                            })
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
                        let mut blocks = msg["content"].as_array().cloned().unwrap_or_else(|| {
                            msg["content"]
                                .as_str()
                                .filter(|text| !text.is_empty())
                                .map(|text| vec![json!({ "type": "text", "text": text })])
                                .unwrap_or_default()
                        });
                        blocks.extend(tool_calls.iter().map(|tc| {
                            let input: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(Value::Object(serde_json::Map::new()));
                            json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            })
                        }));
                        msg["content"] = json!(blocks);
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
        log_debug_request(&request, "Anthropic");
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
        log_debug_request(&request, "Anthropic");
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

        let (receiver, producer) = spawn_anthropic_stream_producer(response.bytes_stream());
        Ok(Box::new(AnthropicStream::new(receiver, producer)))
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

#[cfg(test)]
fn parse_sse_events(text: &str) -> Vec<Result<StreamEvent>> {
    let mut message_start_usage = None;
    let mut current_tool = None;
    text.split("\n\n")
        .filter_map(|event| parse_sse_event(event, &mut message_start_usage, &mut current_tool))
        .collect()
}

fn parse_sse_event(
    event: &str,
    message_start_usage: &mut Option<TokenUsage>,
    current_tool: &mut Option<(String, String)>,
) -> Option<Result<StreamEvent>> {
    let data_line = event
        .split(|character| character == '\r' || character == '\n')
        .find(|line| line.starts_with("data: "))?;
    let json_str = &data_line[6..];
    let parsed = match serde_json::from_str::<Value>(json_str) {
        Ok(parsed) => parsed,
        Err(error) => return Some(Err(ProviderError::Serde(error))),
    };

    match parsed["type"].as_str().unwrap_or("") {
        "message_start" => {
            *message_start_usage = parsed
                .get("message")
                .and_then(|message| message.get("usage"))
                .and_then(parse_token_usage);
            None
        }
        "content_block_start" => {
            let content_block = &parsed["content_block"];
            if content_block["type"].as_str() != Some("tool_use") {
                return None;
            }
            let id = content_block["id"].as_str()?.to_string();
            let name = content_block["name"].as_str()?.to_string();
            *current_tool = Some((id.clone(), name.clone()));
            Some(Ok(StreamEvent::ToolCallDelta {
                id,
                name,
                arguments_delta: String::new(),
            }))
        }
        "content_block_delta" => {
            let delta = &parsed["delta"];
            if delta["type"].as_str() == Some("input_json_delta") {
                let (id, name) = current_tool.as_ref()?.clone();
                return Some(Ok(StreamEvent::ToolCallDelta {
                    id,
                    name,
                    arguments_delta: delta["partial_json"].as_str()?.to_string(),
                }));
            }
            delta["text"]
                .as_str()
                .map(|text| Ok(StreamEvent::TextDelta { text: text.into() }))
        }
        "content_block_stop" => current_tool
            .take()
            .map(|(id, _)| Ok(StreamEvent::ToolCallEnd { id })),
        "message_delta" => {
            let usage = parsed.get("usage").and_then(|delta| {
                let input_is_known =
                    message_start_usage.is_some() || delta.get("input_tokens").is_some();
                if !input_is_known {
                    return None;
                }

                let mut usage = message_start_usage.clone().unwrap_or(TokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                });
                if let Some(input_tokens) = delta.get("input_tokens").and_then(Value::as_u64) {
                    usage.input_tokens = input_tokens;
                }
                if let Some(output_tokens) = delta.get("output_tokens").and_then(Value::as_u64) {
                    usage.output_tokens = output_tokens;
                }
                if let Some(cache_read_input_tokens) =
                    delta.get("cache_read_input_tokens").and_then(Value::as_u64)
                {
                    usage.cache_read_input_tokens = Some(cache_read_input_tokens);
                }
                if let Some(cache_creation_input_tokens) = delta
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                {
                    usage.cache_creation_input_tokens = Some(cache_creation_input_tokens);
                }
                Some(usage)
            });
            Some(Ok(StreamEvent::MessageEnd { usage }))
        }
        "error" => Some(Ok(StreamEvent::Error {
            message: parsed["error"]["message"]
                .as_str()
                .unwrap_or("Anthropic stream error")
                .into(),
        })),
        _ => None,
    }
}

fn parse_token_usage(value: &Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: value.get("input_tokens")?.as_u64()?,
        output_tokens: value["output_tokens"].as_u64().unwrap_or(0),
        cache_read_input_tokens: value["cache_read_input_tokens"].as_u64(),
        cache_creation_input_tokens: value["cache_creation_input_tokens"].as_u64(),
    })
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

    #[test]
    fn multiple_system_messages_are_combined_for_anthropic() {
        let provider = AnthropicProvider::new("test-key".into());
        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".into(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text("base instructions".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text("hello".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text("conversation summary".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            system: Some("request system".into()),
        };

        let body = provider.build_body(&request, false);

        assert_eq!(
            body["system"],
            "request system\n\nbase instructions\n\nconversation summary"
        );
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn assistant_tool_calls_are_always_content_block_arrays() {
        let provider = AnthropicProvider::new("test-key".into());
        let messages = vec![ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text("I will inspect that.".into()),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "tool-1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "file_read".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
        }];

        let converted = provider.convert_messages(&messages);
        let content = converted[0]["content"].as_array().expect("content blocks");

        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "tool-1");
    }

    #[test]
    fn parsed_tool_use_is_serialized_once_on_follow_up() {
        let provider = AnthropicProvider::new("test-key".into());
        let (message, _) = AnthropicProvider::parse_content_blocks(&json!([
            {"type": "text", "text": "I will inspect that."},
            {
                "type": "tool_use",
                "id": "tool-1",
                "name": "file_read",
                "input": {"path": "README.md"}
            }
        ]));

        let converted = provider.convert_messages(&[message]);
        let content = converted[0]["content"].as_array().expect("content blocks");

        assert_eq!(
            content
                .iter()
                .filter(|block| block["type"] == "tool_use")
                .count(),
            1
        );
    }

    #[test]
    fn unmatched_tool_use_parts_are_preserved() {
        let provider = AnthropicProvider::new("test-key".into());
        let messages = vec![ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::ToolUse {
                    id: "tool-keep".into(),
                    name: "file_read".into(),
                    input: json!({"path": "README.md"}),
                },
                ContentPart::ToolUse {
                    id: "tool-unmatched".into(),
                    name: "file_read".into(),
                    input: json!({"path": "Cargo.toml"}),
                },
            ]),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "tool-keep".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "file_read".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
        }];

        let converted = provider.convert_messages(&messages);
        let content = converted[0]["content"].as_array().expect("content blocks");

        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["id"], "tool-unmatched");
        assert_eq!(content[1]["id"], "tool-keep");
    }

    #[test]
    fn stream_usage_combines_message_start_and_delta_usage() {
        let events = parse_sse_events(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11,\"cache_read_input_tokens\":2}}}\n\n\
             data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\n",
        );

        let usage = events
            .into_iter()
            .find_map(|event| match event {
                Ok(StreamEvent::MessageEnd { usage }) => usage,
                _ => None,
            })
            .expect("message end usage");

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_read_input_tokens, Some(2));
    }

    #[test]
    fn stream_usage_is_missing_when_message_start_omits_input_usage() {
        let events = parse_sse_events(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"cache_read_input_tokens\":2}}}\n\n\
             data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\n",
        );

        assert!(matches!(
            events.as_slice(),
            [Ok(StreamEvent::MessageEnd { usage: None })]
        ));
    }

    #[tokio::test]
    async fn stream_accepts_crlf_sse_frames() {
        let bytes = bytes::Bytes::from(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hello\"}}\r\n\r\n\
             data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"world\"}}\r\n\r\n",
        );
        let byte_stream = futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes)]);
        let (mut receiver, producer) = spawn_anthropic_stream_producer(byte_stream);

        assert!(matches!(
            receiver.recv().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "hello"
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "world"
        ));
        assert!(receiver.recv().await.is_none());
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn stream_accepts_bare_cr_sse_lines() {
        let bytes = bytes::Bytes::from(
            "event: content_block_delta\rdata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hello\"}}\r\r",
        );
        let byte_stream = futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes)]);
        let (mut receiver, producer) = spawn_anthropic_stream_producer(byte_stream);

        assert!(matches!(
            receiver.recv().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "hello"
        ));
        assert!(receiver.recv().await.is_none());
        producer.await.unwrap();
    }
}
