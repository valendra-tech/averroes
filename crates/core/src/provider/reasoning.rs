use serde_json::Value;

/// Collect text from one provider reasoning value. Object keys are aliases in
/// the compatible APIs, but some gateways populate more than one alias with
/// the same fragment. Keep distinct values while suppressing exact aliases.
fn reasoning_value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values.iter().map(reasoning_value_text).collect(),
        Value::Object(object) => {
            let mut output = String::new();
            let mut seen = Vec::new();
            for key in ["text", "content", "delta", "summary", "thinking"] {
                if let Some(value) = object.get(key) {
                    append_unique_candidate(&mut output, &mut seen, reasoning_value_text(value));
                }
            }
            output
        }
        _ => String::new(),
    }
}

fn append_unique_candidate(output: &mut String, seen: &mut Vec<String>, candidate: String) {
    if candidate.is_empty() || seen.iter().any(|existing| existing == &candidate) {
        return;
    }
    output.push_str(&candidate);
    seen.push(candidate);
}

pub(crate) fn chat_reasoning(value: &Value) -> Option<String> {
    let mut output = String::new();
    let mut seen = Vec::new();

    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(value) = value.get(key) {
            append_unique_candidate(&mut output, &mut seen, reasoning_value_text(value));
        }
    }

    if let Some(parts) = value.get("content").and_then(Value::as_array) {
        for part in parts {
            let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "reasoning" | "thinking" | "reasoning_content") {
                append_unique_candidate(&mut output, &mut seen, reasoning_value_text(part));
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
                let mut seen = Vec::new();
                for key in ["summary", "content", "text"] {
                    if let Some(value) = item.get(key) {
                        append_unique_candidate(
                            &mut output,
                            &mut seen,
                            reasoning_value_text(value),
                        );
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
    fn duplicate_compatible_aliases_are_emitted_only_once() {
        assert_eq!(
            chat_reasoning(&json!({
                "reasoning_content": "The user is asking",
                "reasoning": "The user is asking"
            })),
            Some("The user is asking".into())
        );
        assert_eq!(
            chat_reasoning(&json!({
                "reasoning": {
                    "text": "Nested fragment",
                    "content": "Nested fragment"
                }
            })),
            Some("Nested fragment".into())
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
