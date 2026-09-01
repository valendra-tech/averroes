use super::{Result, StreamEvent};
use futures::{Stream, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::provider::reasoning::chat_reasoning;
use crate::provider::types::TokenUsage;
use crate::provider::ProviderError;

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
                                            if let Some(reasoning) = chat_reasoning(delta) {
                                                if tx
                                                    .send(Ok(StreamEvent::ReasoningDelta {
                                                        text: reasoning,
                                                    }))
                                                    .is_err()
                                                {
                                                    return;
                                                }
                                            }
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

pub(crate) fn spawn_responses_stream_producer<S>(
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
        let mut active_tool_calls: HashMap<String, (String, String, String)> = HashMap::new();
        while let Some(chunk_result) = byte_stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(newline_pos) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
                        let line = match String::from_utf8(line_bytes) {
                            Ok(line) => line,
                            Err(_) => continue,
                        };
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            continue;
                        }

                        let data = match line.strip_prefix("data: ") {
                            Some(data) => data,
                            None => continue,
                        };

                        let json: Value = match serde_json::from_str(data) {
                            Ok(json) => json,
                            Err(_) => continue,
                        };

                        let event_type = json["type"].as_str().unwrap_or("");

                        match event_type {
                            "response.output_text.delta" => {
                                if let Some(delta) = json["delta"].as_str() {
                                    if !delta.is_empty() {
                                        if tx
                                            .send(Ok(StreamEvent::TextDelta {
                                                text: delta.to_string(),
                                            }))
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                }
                            }
                            "response.reasoning_summary_text.delta" => {
                                if let Some(delta) = json["delta"].as_str() {
                                    if !delta.is_empty() {
                                        if tx
                                            .send(Ok(StreamEvent::ReasoningDelta {
                                                text: delta.to_string(),
                                            }))
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                }
                            }
                            "response.output_item.added" => {
                                if let Some(item) = json.get("item") {
                                    if item.get("type").and_then(Value::as_str)
                                        == Some("function_call")
                                    {
                                        let id = item
                                            .get("id")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        let call_id = item
                                            .get("call_id")
                                            .and_then(Value::as_str)
                                            .unwrap_or(&id)
                                            .to_string();
                                        let name = item
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        let args = item
                                            .get("arguments")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        if !args.is_empty()
                                            && tx
                                                .send(Ok(StreamEvent::ToolCallDelta {
                                                    id: call_id.clone(),
                                                    name: name.clone(),
                                                    arguments_delta: args.clone(),
                                                }))
                                                .is_err()
                                        {
                                            return;
                                        }
                                        active_tool_calls.insert(id, (call_id, name, args));
                                    }
                                }
                            }
                            "response.function_call_arguments.delta" => {
                                if let Some(item_id) = json.get("item_id").and_then(Value::as_str) {
                                    if let Some((call_id, name, args)) =
                                        active_tool_calls.get_mut(item_id)
                                    {
                                        if let Some(delta) =
                                            json.get("delta").and_then(Value::as_str)
                                        {
                                            args.push_str(delta);
                                            if tx
                                                .send(Ok(StreamEvent::ToolCallDelta {
                                                    id: call_id.clone(),
                                                    name: name.clone(),
                                                    arguments_delta: delta.to_string(),
                                                }))
                                                .is_err()
                                            {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                            "response.output_item.done" => {
                                if let Some(item) = json.get("item") {
                                    if item.get("type").and_then(Value::as_str)
                                        == Some("function_call")
                                    {
                                        if let Some(item_id) =
                                            item.get("id").and_then(Value::as_str)
                                        {
                                            let call_id = active_tool_calls
                                                .get(item_id)
                                                .map(|(call_id, _, _)| call_id.as_str())
                                                .unwrap_or(item_id)
                                                .to_string();
                                            if tx
                                                .send(Ok(StreamEvent::ToolCallEnd { id: call_id }))
                                                .is_err()
                                            {
                                                return;
                                            }
                                            active_tool_calls.remove(item_id);
                                        }
                                    }
                                }
                            }
                            "response.completed" => {
                                if let Some(resp) = json.get("response") {
                                    let usage = resp.get("usage").map(|u| TokenUsage {
                                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                                        cache_read_input_tokens: u
                                            .get("input_tokens_details")
                                            .and_then(|d| d.get("cached_tokens"))
                                            .and_then(Value::as_u64),
                                        cache_creation_input_tokens: None,
                                    });
                                    if tx.send(Ok(StreamEvent::MessageEnd { usage })).is_err() {
                                        return;
                                    }
                                } else if tx
                                    .send(Ok(StreamEvent::MessageEnd { usage: None }))
                                    .is_err()
                                {
                                    return;
                                }
                                return;
                            }
                            _ => {}
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

#[cfg(test)]
mod tests {
    use crate::provider::reasoning::chat_reasoning;
    use serde_json::json;

    #[test]
    fn extracts_all_openai_compatible_reasoning_shapes() {
        assert_eq!(
            chat_reasoning(&json!({"reasoning_content": "first", "reasoning": " second"})),
            Some("first second".into())
        );
        assert_eq!(
            chat_reasoning(&json!({
                "content": [{"type": "reasoning", "text": "typed summary"}]
            })),
            Some("typed summary".into())
        );
    }

    #[test]
    fn ignores_normal_content_when_extracting_reasoning() {
        assert!(chat_reasoning(&json!({"content": "answer"})).is_none());
    }
}
