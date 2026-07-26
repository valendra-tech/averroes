use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::{
    log_debug_request, ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, Result,
    StreamEvent,
};
use crate::provider::types::{
    ChatMessage, ContentPart, FunctionCall, MessageContent, Role, TokenUsage, ToolCall,
};

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    context_windows: Vec<(String, usize)>,
}

pub(crate) struct OpenAiStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Result<StreamEvent>>,
    producer: Option<tokio::task::JoinHandle<()>>,
}

impl OpenAiStream {
    pub(crate) fn new(
        receiver: tokio::sync::mpsc::UnboundedReceiver<Result<StreamEvent>>,
        producer: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            receiver,
            producer: Some(producer),
        }
    }
}

impl Unpin for OpenAiStream {}

impl Stream for OpenAiStream {
    type Item = Result<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

impl Drop for OpenAiStream {
    fn drop(&mut self) {
        if let Some(producer) = self.producer.take() {
            producer.abort();
        }
    }
}

pub(crate) fn spawn_openai_stream_producer<S>(
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
        let mut active_tool_calls = HashMap::<u64, (String, String)>::new();
        while let Some(chunk_result) = byte_stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(newline_pos) = buffer.iter().position(|byte| *byte == b'\n') {
                        let mut line_bytes = buffer.drain(..=newline_pos).collect::<Vec<_>>();
                        line_bytes.pop();
                        let line = match String::from_utf8(line_bytes) {
                            Ok(line) => line,
                            Err(error) => {
                                let _ = tx.send(Err(ProviderError::Other(format!(
                                    "Invalid UTF-8 in OpenAI stream: {error}"
                                ))));
                                return;
                            }
                        };
                        let line = line.trim();

                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                if !emit_openai_tool_call_ends(&tx, &mut active_tool_calls) {
                                    return;
                                }
                                if tx
                                    .send(Ok(StreamEvent::MessageEnd { usage: None }))
                                    .is_err()
                                {
                                    return;
                                }
                                return;
                            }
                            match serde_json::from_str::<Value>(data) {
                                Ok(json) => {
                                    if json.get("error").is_some() {
                                        let message = json["error"]["message"]
                                            .as_str()
                                            .unwrap_or("OpenAI stream error")
                                            .to_string();
                                        if tx.send(Ok(StreamEvent::Error { message })).is_err() {
                                            return;
                                        }
                                        return;
                                    }
                                    if json["choices"]
                                        .as_array()
                                        .is_some_and(|choices| choices.is_empty())
                                    {
                                        if !emit_openai_tool_call_ends(&tx, &mut active_tool_calls)
                                        {
                                            return;
                                        }
                                        let usage = json.get("usage").map(|usage| TokenUsage {
                                            input_tokens: usage["prompt_tokens"]
                                                .as_u64()
                                                .unwrap_or(0),
                                            output_tokens: usage["completion_tokens"]
                                                .as_u64()
                                                .unwrap_or(0),
                                            cache_read_input_tokens: usage
                                                .get("cache_read_input_tokens")
                                                .and_then(Value::as_u64),
                                            cache_creation_input_tokens: usage
                                                .get("cache_creation_input_tokens")
                                                .and_then(Value::as_u64),
                                        });
                                        if tx.send(Ok(StreamEvent::MessageEnd { usage })).is_err() {
                                            return;
                                        }
                                        return;
                                    }
                                    if let Some(choices) = json["choices"].as_array() {
                                        for choice in choices {
                                            let delta = &choice["delta"];
                                            if let Some(content) = delta["content"].as_str() {
                                                if !content.is_empty() {
                                                    if tx
                                                        .send(Ok(StreamEvent::TextDelta {
                                                            text: content.to_string(),
                                                        }))
                                                        .is_err()
                                                    {
                                                        return;
                                                    }
                                                }
                                            }
                                            if let Some(tool_calls) = delta["tool_calls"].as_array()
                                            {
                                                for tc in tool_calls {
                                                    let Some(index) = tc["index"].as_u64() else {
                                                        continue;
                                                    };
                                                    let (id, name) =
                                                        active_tool_calls.entry(index).or_default();
                                                    if let Some(value) = tc["id"].as_str() {
                                                        if !value.is_empty() {
                                                            *id = value.to_string();
                                                        }
                                                    }
                                                    if let Some(value) =
                                                        tc["function"]["name"].as_str()
                                                    {
                                                        if !value.is_empty() {
                                                            *name = value.to_string();
                                                        }
                                                    }
                                                    let args = tc["function"]["arguments"]
                                                        .as_str()
                                                        .unwrap_or("");

                                                    if !id.is_empty()
                                                        || !name.is_empty()
                                                        || !args.is_empty()
                                                    {
                                                        if tx
                                                            .send(Ok(StreamEvent::ToolCallDelta {
                                                                id: id.clone(),
                                                                name: name.clone(),
                                                                arguments_delta: args.to_string(),
                                                            }))
                                                            .is_err()
                                                        {
                                                            return;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(choice) = choices.first() {
                                            let fr = choice["finish_reason"].as_str().unwrap_or("");
                                            if !fr.is_empty() && fr != "null" {
                                                if !emit_openai_tool_call_ends(
                                                    &tx,
                                                    &mut active_tool_calls,
                                                ) {
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    if tx.send(Err(ProviderError::Serde(e))).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    if tx.send(Err(ProviderError::Http(e))).is_err() {
                        return;
                    }
                    break;
                }
            }
        }
    });

    (rx, producer)
}

pub(crate) fn emit_openai_tool_call_ends(
    tx: &tokio::sync::mpsc::UnboundedSender<Result<StreamEvent>>,
    active_tool_calls: &mut HashMap<u64, (String, String)>,
) -> bool {
    let ids = active_tool_calls
        .values()
        .filter_map(|(id, _)| (!id.is_empty()).then(|| id.clone()))
        .collect::<Vec<_>>();
    active_tool_calls.clear();

    for id in ids {
        if tx.send(Ok(StreamEvent::ToolCallEnd { id })).is_err() {
            return false;
        }
    }

    true
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

pub(crate) fn role_system_message(msg: &ChatMessage) -> Value {
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

pub(crate) fn role_user_message(msg: &ChatMessage) -> Value {
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

pub(crate) fn role_assistant_message(msg: &ChatMessage) -> Value {
    let mut obj = json!({ "role": "assistant" });
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(existing_tool_calls) = &msg.tool_calls {
        for tool_call in existing_tool_calls {
            if !tool_calls
                .iter()
                .any(|existing| existing.id == tool_call.id)
            {
                tool_calls.push(tool_call.clone());
            }
        }
    }

    match &msg.content {
        MessageContent::Text(text) => {
            obj["content"] = json!(text);
        }
        MessageContent::Parts(parts) => {
            let mut content = Vec::new();
            for part in parts {
                match part {
                    ContentPart::ToolUse { id, name, input } => {
                        if !tool_calls.iter().any(|tool_call| tool_call.id == *id) {
                            tool_calls.push(ToolCall {
                                id: id.clone(),
                                call_type: "function".into(),
                                function: FunctionCall {
                                    name: name.clone(),
                                    arguments: input.to_string(),
                                },
                            });
                        }
                    }
                    ContentPart::ToolResult { .. } => {}
                    _ => content.push(convert_content_part(part)),
                }
            }
            obj["content"] = if content.is_empty() {
                Value::Null
            } else {
                Value::Array(content)
            };
        }
    }

    if !tool_calls.is_empty() {
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

pub(crate) fn role_tool_message(msg: &ChatMessage) -> Value {
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

pub(crate) fn convert_content_part(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({
            "type": "text",
            "text": text,
        }),
        ContentPart::Image { source } => {
            let data_url = format!("data:{};base64,{}", source.media_type, source.data);
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
        ContentPart::ToolResult {
            tool_use_id,
            content,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }),
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        log_debug_request(&request, "OpenAI");
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
        log_debug_request(&request, "OpenAI");
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

        let stream_body = provider.build_body(&request, true);
        assert_eq!(stream_body["stream_options"]["include_usage"], true);
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
            max_tokens: None,
            temperature: None,
            system: None,
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
            max_tokens: None,
            temperature: None,
            system: None,
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
}
