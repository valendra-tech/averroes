use super::Result;
use crate::provider::types::TokenUsage;
use crate::provider::{ProviderError, StreamEvent};
use futures::{Stream, StreamExt};
use serde_json::Value;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(super) struct AnthropicStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Result<StreamEvent>>,
    producer: Option<tokio::task::JoinHandle<()>>,
}

impl AnthropicStream {
    pub(super) fn new(
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

pub(super) fn spawn_anthropic_stream_producer<S>(
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

#[cfg(test)]
pub(super) fn parse_sse_events(text: &str) -> Vec<Result<StreamEvent>> {
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
            if delta["type"].as_str() == Some("thinking_delta") {
                return delta["thinking"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(|text| Ok(StreamEvent::ReasoningDelta { text: text.into() }));
            }
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
                    reasoning_output_tokens: None,
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
        reasoning_output_tokens: None,
    })
}
