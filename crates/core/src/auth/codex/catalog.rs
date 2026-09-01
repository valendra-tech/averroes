use super::CodexModel;
use serde_json::Value;
use std::collections::HashSet;

/// Normalizes the several catalog shapes returned by ChatGPT's account API.
/// This parser deliberately lives outside the OAuth and transport code so a
/// catalog rollout can evolve without touching authentication.
pub(super) fn parse_models(value: &Value) -> Vec<CodexModel> {
    let mut entries = Vec::new();
    collect_model_entries(value, 0, &mut entries);
    let mut seen_ids = HashSet::new();

    entries
        .into_iter()
        .filter_map(|(indexed_id, model)| {
            let id = [
                "slug",
                "id",
                "model",
                "model_id",
                "modelId",
                "model_slug",
                "modelSlug",
                "name",
            ]
            .into_iter()
            .find_map(|key| model.get(key).and_then(Value::as_str))
            .or(indexed_id)?
            .trim();
            if id.is_empty()
                || model.get("supported_in_api").and_then(Value::as_bool) == Some(false)
                || !seen_ids.insert(id.to_owned())
            {
                return None;
            }
            let reasoning_efforts = [
                "supported_reasoning_levels",
                "supportedReasoningLevels",
                "reasoning_efforts",
                "reasoningEfforts",
                "reasoning_levels",
            ]
            .into_iter()
            .find_map(|key| model.get(key))
            .map(|levels| match levels {
                Value::Array(levels) => levels
                    .iter()
                    .filter_map(|level| {
                        level.as_str().map(str::to_owned).or_else(|| {
                            level
                                .get("effort")
                                .or_else(|| level.get("value"))
                                .or_else(|| level.get("name"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                    })
                    .filter(|effort| !effort.trim().is_empty())
                    .collect(),
                Value::String(level) if !level.trim().is_empty() => vec![level.clone()],
                _ => Vec::new(),
            })
            .unwrap_or_default();
            Some(CodexModel {
                id: id.to_string(),
                display_name: model
                    .get("display_name")
                    .or_else(|| model.get("displayName"))
                    .or_else(|| model.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                description: model
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                reasoning_efforts,
            })
        })
        .collect()
}

fn collect_model_entries<'a>(
    value: &'a Value,
    depth: usize,
    entries: &mut Vec<(Option<&'a str>, &'a Value)>,
) {
    if depth > 4 {
        return;
    }

    match value {
        Value::Array(values) => {
            for value in values {
                if value.as_str().is_some() {
                    entries.push((value.as_str(), value));
                } else if is_model_entry(value) {
                    entries.push((None, value));
                } else {
                    collect_model_entries(value, depth + 1, entries);
                }
            }
        }
        Value::Object(values) => {
            if is_model_entry(value) {
                entries.push((None, value));
            } else {
                for (key, value) in values {
                    if is_indexed_model_entry(value) {
                        entries.push((Some(key.as_str()), value));
                    } else {
                        collect_model_entries(value, depth + 1, entries);
                    }
                }
            }
        }
        _ => {}
    }
}

fn is_model_entry(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    [
        "slug",
        "id",
        "model",
        "model_id",
        "modelId",
        "model_slug",
        "modelSlug",
        "name",
    ]
    .into_iter()
    .any(|key| object.get(key).and_then(Value::as_str).is_some())
}

fn is_indexed_model_entry(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    [
        "display_name",
        "displayName",
        "description",
        "supported_in_api",
        "supported_reasoning_levels",
        "supportedReasoningLevels",
        "reasoning_efforts",
        "reasoningEfforts",
        "reasoning_levels",
        "capabilities",
    ]
    .into_iter()
    .any(|key| object.contains_key(key))
}
