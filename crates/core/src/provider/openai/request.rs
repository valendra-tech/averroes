use super::ChatRequest;
use crate::provider::model_uses_reasoning_api;
use crate::provider::types::{
    ChatMessage as ProviderChatMessage, ContentPart, FunctionCall, MessageContent, Role, ToolCall,
};
use serde_json::{json, Value};

pub(super) fn build_chat_body(request: &ChatRequest, stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = &request.system {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }

    for msg in &request.messages {
        match msg.role {
            Role::System => messages.push(role_system_message(msg)),
            Role::User => messages.push(role_user_message(msg)),
            Role::Assistant => messages.push(role_assistant_message(msg)),
            Role::Tool => messages.push(role_tool_message(msg)),
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

pub(super) fn build_responses_body(request: &ChatRequest, stream: bool) -> Value {
    let mut input: Vec<Value> = Vec::new();

    for msg in &request.messages {
        let role_str = match msg.role {
            Role::System => "developer",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "user",
        };

        let content = match &msg.content {
            MessageContent::Text(text) => json!(text),
            MessageContent::Parts(parts) => {
                let blocks: Vec<Value> = parts
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => {
                            json!({"type": "input_text", "text": text})
                        }
                        ContentPart::Image { source } => json!({
                            "type": "input_image",
                            "image_url": format!("data:{};base64,{}", source.media_type, source.data)
                        }),
                        ContentPart::ToolUse { id, name, input } => json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": input.to_string(),
                        }),
                        ContentPart::ToolResult { tool_use_id, content } => json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": content,
                        }),
                    })
                    .collect();
                Value::Array(blocks)
            }
        };

        if msg.role == Role::Tool {
            if let Some(tool_call_id) = &msg.tool_call_id {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": match &msg.content {
                        MessageContent::Text(text) => json!(text),
                        _ => json!(""),
                    },
                }));
            }
            continue;
        }

        if msg.role == Role::Assistant {
            if let Some(tool_calls) = &msg.tool_calls {
                if let MessageContent::Text(text) = &msg.content {
                    if !text.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": text,
                        }));
                    }
                }
                for tc in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.function.name,
                        "arguments": tc.function.arguments,
                    }));
                }
                continue;
            }
        }

        input.push(json!({
            "role": role_str,
            "content": content,
        }));
    }

    let mut body = json!({
        "model": request.model,
        "input": input,
        "stream": stream,
    });

    if let Some(effort) = request
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        body["reasoning"] = json!({"effort": effort, "summary": "auto"});
    }

    if let Some(system) = request.system.as_deref().filter(|text| !text.is_empty()) {
        body["instructions"] = json!(system);
    }

    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    })
                })
                .collect(),
        );
    }

    body
}

pub(crate) fn role_system_message(msg: &ProviderChatMessage) -> Value {
    match &msg.content {
        MessageContent::Text(text) => json!({"role": "system", "content": text}),
        MessageContent::Parts(parts) => json!({
            "role": "system",
            "content": parts.iter().map(convert_content_part).collect::<Vec<_>>(),
        }),
    }
}

pub(crate) fn role_user_message(msg: &ProviderChatMessage) -> Value {
    match &msg.content {
        MessageContent::Text(text) => json!({"role": "user", "content": text}),
        MessageContent::Parts(parts) => json!({
            "role": "user",
            "content": parts.iter().map(convert_content_part).collect::<Vec<_>>(),
        }),
    }
}

pub(crate) fn role_assistant_message(msg: &ProviderChatMessage) -> Value {
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
        MessageContent::Text(text) => obj["content"] = json!(text),
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
                .map(|tool_call| {
                    json!({
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.function.name,
                            "arguments": tool_call.function.arguments,
                        }
                    })
                })
                .collect(),
        );
    }

    obj
}

pub(crate) fn role_tool_message(msg: &ProviderChatMessage) -> Value {
    let mut obj = json!({ "role": "tool" });
    let content = match &msg.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    };
    obj["content"] = json!(content);
    if let Some(tool_call_id) = &msg.tool_call_id {
        obj["tool_call_id"] = json!(tool_call_id);
    }
    obj
}

pub(crate) fn convert_content_part(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::Image { source } => json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", source.media_type, source.data),
            }
        }),
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
