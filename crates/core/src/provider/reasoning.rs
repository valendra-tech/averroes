use serde_json::Value;

/// Collect text from a provider's reasoning-shaped JSON value without ever
/// imposing a character limit. Provider APIs disagree on the exact nesting:
/// some use a string, others use arrays of summary parts.
pub(crate) fn append_reasoning_value(value: &Value, output: &mut String) {
    match value {
        Value::String(text) => output.push_str(text),
        Value::Array(values) => {
            for value in values {
                append_reasoning_value(value, output);
            }
        }
        Value::Object(object) => {
            for key in ["text", "content", "delta", "summary", "thinking"] {
                if let Some(value) = object.get(key) {
                    append_reasoning_value(value, output);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn chat_reasoning(value: &Value) -> Option<String> {
    let mut output = String::new();

    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(value) = value.get(key) {
            append_reasoning_value(value, &mut output);
        }
    }

    if let Some(parts) = value.get("content").and_then(Value::as_array) {
        for part in parts {
            let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "reasoning" | "thinking" | "reasoning_content") {
                append_reasoning_value(part, &mut output);
            }
        }
    }

    (!output.is_empty()).then_some(output)
}

pub(crate) fn responses_reasoning(value: &Value) -> Option<String> {
    let mut output = String::new();

    if let Some(items) = value.get("output").and_then(Value::as_array) {
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                for key in ["summary", "content", "text"] {
                    if let Some(value) = item.get(key) {
                        append_reasoning_value(value, &mut output);
                    }
                }
            }
        }
    }

    (!output.is_empty()).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::{chat_reasoning, responses_reasoning};
    use serde_json::json;

    #[test]
    fn reads_compatible_chat_reasoning_fields() {
        assert_eq!(
            chat_reasoning(&json!({"reasoning_content": "one", "thinking": " two"})),
            Some("one two".into())
        );
    }

    #[test]
    fn reads_responses_reasoning_summary_parts() {
        assert_eq!(
            responses_reasoning(&json!({
                "output": [{
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "First"},
                        {"type": "summary_text", "text": "Second"}
                    ]
                }]
            })),
            Some("FirstSecond".into())
        );
    }
}
