//! QDivZero provider-specific catalog and endpoint configuration.

use serde_json::Value;
use std::collections::HashMap;

use super::{ProviderModel, ProviderModelKind};
use crate::connection::ConnectionKind;
use crate::provider::hooks::{ModelDiscovery, ProviderRegistry, StandardProviderHook};

/// Root URL for QDivZero's public API.
pub use crate::connection::QDIVZERO_BASE_URL;

pub(crate) fn register_provider_hook(registry: &mut ProviderRegistry) {
    registry.register(StandardProviderHook::new(
        "qdivzero",
        ConnectionKind::QDivZero,
        Some("qdivzero"),
        ModelDiscovery::RemoteApi,
    ));
}

/// Parses QDivZero's `GET /serving-endpoints` response.
pub fn parse_serving_endpoints(value: &Value) -> Vec<ProviderModel> {
    let endpoints: Vec<&Value> = if let Some(endpoints) = value.as_array() {
        endpoints.iter().collect()
    } else if let Some(endpoints) = value.get("endpoints").and_then(Value::as_array) {
        endpoints.iter().collect()
    } else if let Some(endpoints) = value.get("data").and_then(Value::as_array) {
        endpoints.iter().collect()
    } else {
        // The documented API returns an object whose values are endpoint
        // arrays, grouped by account/tenant. Flatten those groups without
        // coupling the client to the grouping key.
        value
            .as_object()
            .into_iter()
            .flat_map(|object| object.values())
            .filter_map(Value::as_array)
            .flatten()
            .collect()
    };
    let mut models: Vec<ProviderModel> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();

    for endpoint in endpoints {
        // QDivZero uses `id` for the serving-endpoint resource. Instance
        // endpoints therefore expose an internal UUID there, while `name`
        // is the identifier accepted by `/v1/chat/completions` and
        // `/v1/embeddings`. Public endpoints happen to use the same value in
        // both fields, so preferring `name` works for both shapes.
        let Some(id) = ["name", "id"].into_iter().find_map(|field| {
            endpoint
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
        }) else {
            continue;
        };
        let kind = match endpoint
            .get("workload_kind")
            .and_then(Value::as_str)
            .map(|kind| kind.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("chat") => ProviderModelKind::Chat,
            Some("embedding" | "embeddings") => ProviderModelKind::Embedding,
            _ => continue,
        };
        // The OpenAPI schema leaves these fields optional. `state` is the
        // authoritative readiness signal; an omitted `enabled` or protocol
        // list must not hide an otherwise usable running endpoint.
        let is_running = endpoint
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.trim().eq_ignore_ascii_case("running"));
        let is_enabled = endpoint
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let supports_chat_completions = endpoint
            .get("supported_protocols")
            .and_then(Value::as_array)
            .is_none_or(|protocols| {
                protocols.is_empty()
                    || protocols.iter().filter_map(Value::as_str).any(|protocol| {
                        let protocol = protocol.trim().to_ascii_lowercase();
                        match kind {
                            ProviderModelKind::Chat => {
                                matches!(
                                    protocol.as_str(),
                                    "chat_completions" | "/v1/chat/completions"
                                )
                            }
                            ProviderModelKind::Embedding => {
                                matches!(
                                    protocol.as_str(),
                                    "embeddings" | "embedding" | "/v1/embeddings"
                                )
                            }
                            ProviderModelKind::Both => false,
                        }
                    })
            });
        if !(is_running && is_enabled && supports_chat_completions) {
            continue;
        }

        let owned_by = endpoint
            .get("providers")
            .and_then(Value::as_array)
            .and_then(|providers| providers.first())
            .and_then(|provider| provider.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned);

        if let Some(&position) = positions.get(id) {
            if models[position].kind != Some(kind) {
                models[position].kind = Some(ProviderModelKind::Both);
            }
            continue;
        }

        positions.insert(id.to_owned(), models.len());
        models.push(ProviderModel {
            id: id.to_owned(),
            owned_by,
            kind: Some(kind),
        });
    }

    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_only_running_enabled_endpoints_with_their_advertised_capability() {
        let models = parse_serving_endpoints(&json!([
            {
                "id": "nvidia/Qwen3.6-35B-A3B-NVFP4",
                "display_name": "Qwen 3.6",
                "workload_kind": "chat",
                "enabled": true,
                "state": "running",
                "supported_protocols": ["chat_completions"],
                "providers": [{"name": "nvidia"}]
            },
            {
                "id": "public:gpt-oss-safeguard-120b-european",
                "workload_kind": "chat",
                "enabled": true,
                "state": "running",
                "supported_protocols": ["chat_completions"]
            },
            {
                "id": "old-model",
                "workload_kind": "chat",
                "enabled": true,
                "state": "stopped",
                "supported_protocols": ["chat_completions"]
            },
            {
                "id": "disabled-model",
                "workload_kind": "chat",
                "enabled": false,
                "state": "running",
                "supported_protocols": ["chat_completions"]
            },
            {
                "id": "qdiv-embed-v1",
                "workload_kind": "embedding",
                "enabled": true,
                "state": "running",
                "supported_protocols": ["embeddings"]
            },
            {
                "id": "non-chat-transport",
                "workload_kind": "chat",
                "enabled": true,
                "state": "running",
                "supported_protocols": ["responses"]
            }
        ]));

        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "nvidia/Qwen3.6-35B-A3B-NVFP4");
        assert_eq!(models[0].kind, Some(ProviderModelKind::Chat));
        assert_eq!(models[1].id, "public:gpt-oss-safeguard-120b-european");
        assert_eq!(models[1].kind, Some(ProviderModelKind::Chat));
        assert_eq!(models[2].id, "qdiv-embed-v1");
        assert_eq!(models[2].kind, Some(ProviderModelKind::Embedding));
        assert_eq!(models[0].owned_by.as_deref(), Some("nvidia"));
    }

    #[test]
    fn accepts_a_top_level_object_and_deduplicates_endpoint_ids() {
        let models = parse_serving_endpoints(&json!({
            "account-a": [
                {
                    "id": "model-a",
                    "workload_kind": "chat",
                    "enabled": true,
                    "state": "running",
                    "supported_protocols": ["chat_completions"]
                },
                {
                    "id": "model-a",
                    "workload_kind": "chat",
                    "enabled": true,
                    "state": "running",
                    "supported_protocols": ["chat_completions"]
                },
                {
                    "id": "model-a",
                    "workload_kind": "embedding",
                    "enabled": true,
                    "state": "running",
                    "supported_protocols": ["embeddings"]
                }
            ]
        }));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "model-a");
        assert_eq!(models[0].kind, Some(ProviderModelKind::Both));
    }

    #[test]
    fn accepts_running_chat_endpoints_when_optional_metadata_is_omitted() {
        let models = parse_serving_endpoints(&json!([
            {
                "id": "public:chat-without-optional-flags",
                "workload_kind": "chat",
                "state": "RUNNING"
            }
        ]));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "public:chat-without-optional-flags");
        assert_eq!(models[0].kind, Some(ProviderModelKind::Chat));
    }

    #[test]
    fn uses_the_serving_name_when_endpoint_id_is_an_internal_uuid() {
        let models = parse_serving_endpoints(&json!([
            {
                "id": "01a03d26-e310-7c1d-8c32-5384fc43235c",
                "name": "radixark/qwen3.8-27b-nvfp4-bf16-lmhead",
                "workload_kind": "chat",
                "enabled": true,
                "state": "running",
                "supported_protocols": ["chat_completions"]
            }
        ]));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "radixark/qwen3.8-27b-nvfp4-bf16-lmhead");
    }
}
