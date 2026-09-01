use super::types::{ModelCapabilities, ModelInfo, ModelSource, ProviderModel, ProviderModelKind};

static GPT5_ALL_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
static GPT5_NO_MAX: &[&str] = &["none", "low", "medium", "high", "xhigh"];
static GPT5_BASIC: &[&str] = &["none", "low", "medium", "high"];
static DEEPSEEK_EFFORTS: &[&str] = &["high"];

pub fn curated_models(provider: &str) -> Vec<ModelInfo> {
    match provider {
        "anthropic" => vec![
            curated_model(
                provider,
                "claude-fable-5",
                "Claude Fable 5",
                "Next-generation intelligence for agents",
                true,
                true,
                true,
                None,
                &[],
            ),
            curated_model(
                provider,
                "claude-opus-5",
                "Claude Opus 5",
                "Complex agentic coding and enterprise",
                true,
                true,
                true,
                None,
                &[],
            ),
            curated_model(
                provider,
                "claude-sonnet-5",
                "Claude Sonnet 5",
                "Best combination of speed and intelligence",
                true,
                true,
                true,
                None,
                &[],
            ),
        ],
        "deepseek" => vec![
            curated_model(
                provider,
                "deepseek-v4-pro",
                "DeepSeek V4 Pro",
                "Flagship reasoning model for complex work",
                false,
                true,
                true,
                Some("high"),
                DEEPSEEK_EFFORTS,
            ),
            curated_model(
                provider,
                "deepseek-v4-flash",
                "DeepSeek V4 Flash",
                "Fast reasoning model for everyday work",
                false,
                true,
                true,
                Some("high"),
                DEEPSEEK_EFFORTS,
            ),
            curated_model(
                provider,
                "deepseek-v4-flash-vision-exp",
                "DeepSeek V4 Flash Vision",
                "Experimental vision-capable DeepSeek model",
                true,
                true,
                false,
                Some("high"),
                DEEPSEEK_EFFORTS,
            ),
        ],
        // Ollama only exposes models that are installed locally. The live
        // catalog is populated from its native /api/tags endpoint.
        // Groq likewise uses its live OpenAI-compatible catalog so stale
        // model IDs are never presented to the user.
        "groq" | "ollama" | "ollama-cloud" | "qdivzero" => Vec::new(),
        _ => vec![
            curated_model(
                provider,
                "gpt-5.6-sol",
                "GPT-5.6 Sol",
                "Flagship for complex professional work",
                true,
                true,
                true,
                Some("medium"),
                GPT5_ALL_EFFORTS,
            ),
            curated_model(
                provider,
                "gpt-5.6-terra",
                "GPT-5.6 Terra",
                "Balanced intelligence and cost",
                true,
                true,
                true,
                Some("medium"),
                GPT5_ALL_EFFORTS,
            ),
            curated_model(
                provider,
                "gpt-5.6-luna",
                "GPT-5.6 Luna",
                "Cost-sensitive high-volume workloads",
                true,
                true,
                true,
                Some("medium"),
                GPT5_ALL_EFFORTS,
            ),
            curated_model(
                provider,
                "gpt-5.4",
                "GPT-5.4",
                "Frontier model for complex work",
                true,
                true,
                true,
                Some("medium"),
                GPT5_NO_MAX,
            ),
            curated_model(
                provider,
                "gpt-5.4-mini",
                "GPT-5.4 mini",
                "Strongest mini for coding and subagents",
                true,
                true,
                false,
                Some("medium"),
                GPT5_NO_MAX,
            ),
            curated_model(
                provider,
                "gpt-5.3-codex",
                "GPT-5.3 Codex",
                "Agentic coding model",
                true,
                true,
                false,
                Some("medium"),
                GPT5_BASIC,
            ),
            curated_model(
                provider,
                "gpt-5",
                "GPT-5",
                "Reasoning model with configurable effort",
                true,
                true,
                false,
                Some("medium"),
                GPT5_BASIC,
            ),
            curated_model(
                provider,
                "gpt-5-mini",
                "GPT-5 mini",
                "Fast and affordable reasoning",
                true,
                true,
                false,
                Some("medium"),
                GPT5_BASIC,
            ),
        ],
    }
}

/// Embedding models are registered alongside chat models, but use their own
/// capability so each picker can show only models that can perform its job.
pub fn curated_embedding_models(provider: &str) -> Vec<ModelInfo> {
    match provider {
        "openai" => vec![
            curated_embedding_model(
                provider,
                "text-embedding-3-small",
                "text-embedding-3-small",
                "Fast, cost-efficient text embeddings",
            ),
            curated_embedding_model(
                provider,
                "text-embedding-3-large",
                "text-embedding-3-large",
                "Higher-quality text embeddings",
            ),
        ],
        _ => Vec::new(),
    }
}

pub fn merge_catalog(
    provider: &str,
    selected_model: &str,
    live: &[ProviderModel],
) -> Vec<ModelInfo> {
    let mut catalog = merge_live_catalog(provider, selected_model, live);
    catalog.retain(|model| model.capabilities.chat);
    sort_catalog(&mut catalog);
    catalog
}

/// Builds a catalog exclusively from models advertised by the provider.
/// Curated entries are used only as metadata overlays for matching live IDs;
/// they are never inserted on their own.
pub fn merge_live_catalog(
    provider: &str,
    selected_model: &str,
    live: &[ProviderModel],
) -> Vec<ModelInfo> {
    let mut catalog: Vec<ModelInfo> = Vec::new();

    for live_model in live {
        let (is_chat, is_embedding) = match live_model.kind {
            Some(ProviderModelKind::Chat) => (true, false),
            Some(ProviderModelKind::Embedding) => (false, true),
            Some(ProviderModelKind::Both) => (true, true),
            None => (
                is_chat_compatible_id(&live_model.id),
                is_embedding_id(&live_model.id),
            ),
        };
        if !is_chat && !is_embedding {
            continue;
        }

        if catalog.iter().any(|model| model.id == live_model.id) {
            continue;
        }

        let mut model = curated_models(provider)
            .into_iter()
            .chain(curated_embedding_models(provider))
            .find(|model| model.id == live_model.id)
            .unwrap_or_else(|| ModelInfo {
                display_name: live_model.id.clone(),
                id: live_model.id.clone(),
                provider: provider.to_string(),
                description: None,
                capabilities: ModelCapabilities {
                    chat: is_chat,
                    embeddings: is_embedding,
                    vision: false,
                    tools: is_chat,
                },
                source: ModelSource::Live,
                featured: false,
                default_reasoning_effort: None,
                available_reasoning_efforts: vec![],
            });
        model.source = ModelSource::Live;
        model.featured = false;
        model.capabilities.chat = is_chat;
        model.capabilities.embeddings = is_embedding;
        model.capabilities.tools = is_chat;
        catalog.push(model);
    }

    if !selected_model.trim().is_empty()
        && is_chat_compatible_id(selected_model)
        && !catalog.iter().any(|model| model.id == selected_model)
    {
        catalog.push(ModelInfo {
            id: selected_model.to_string(),
            display_name: selected_model.to_string(),
            provider: provider.to_string(),
            description: None,
            capabilities: ModelCapabilities {
                chat: true,
                embeddings: false,
                vision: false,
                tools: true,
            },
            source: ModelSource::Manual,
            featured: false,
            default_reasoning_effort: None,
            available_reasoning_efforts: vec![],
        });
    }

    sort_catalog(&mut catalog);
    catalog
}

fn sort_catalog(catalog: &mut [ModelInfo]) {
    catalog.sort_by(|left, right| {
        catalog_rank(left)
            .cmp(&catalog_rank(right))
            .then_with(|| {
                left.display_name
                    .to_ascii_lowercase()
                    .cmp(&right.display_name.to_ascii_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

pub fn filter_models<'a>(models: &'a [ModelInfo], query: &str) -> Vec<&'a ModelInfo> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return models.iter().collect();
    }

    models
        .iter()
        .filter(|model| {
            model.display_name.to_ascii_lowercase().contains(&query)
                || model.id.to_ascii_lowercase().contains(&query)
                || model.provider.to_ascii_lowercase().contains(&query)
        })
        .collect()
}

pub fn parse_provider_models(value: &serde_json::Value) -> Vec<ProviderModel> {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.trim();
            let policy_enabled = model
                .get("policy")
                .and_then(|policy| policy.get("state"))
                .and_then(serde_json::Value::as_str)
                != Some("disabled");
            let supported_by_our_transport = model
                .get("supported_endpoints")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|endpoints| {
                    endpoints.iter().any(|endpoint| {
                        endpoint.as_str().is_some_and(|endpoint| {
                            matches!(endpoint, "/chat/completions" | "/responses")
                        })
                    })
                });
            // `model_picker_enabled` is no longer reliable in Copilot's live
            // response: GitHub can mark every model false while continuing to
            // serve those same account-entitled models. Policy state and an
            // explicit supported route remain authoritative.
            (!id.is_empty() && policy_enabled && supported_by_our_transport).then(|| {
                ProviderModel {
                    id: id.to_string(),
                    owned_by: model
                        .get("owned_by")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    kind: None,
                }
            })
        })
        .collect()
}

pub fn provider_label(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI",
        "anthropic" => "Anthropic",
        "deepseek" => "DeepSeek",
        "groq" => "Groq",
        "ollama" => "Ollama",
        "ollama-cloud" => "Ollama Cloud",
        "qdivzero" => "QDivZero",
        "generic" => "Generic",
        _ => "Provider",
    }
}

pub fn model_uses_reasoning_api(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("o1") || model.starts_with("o3") || model.starts_with("o4")
}

pub fn model_uses_responses_api(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-5") && !model.starts_with("gpt-5-mini")
}

fn curated_model(
    provider: &str,
    id: &str,
    display_name: &str,
    description: &str,
    vision: bool,
    tools: bool,
    featured: bool,
    default_reasoning_effort: Option<&str>,
    available_reasoning_efforts: &[&str],
) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        provider: provider.to_string(),
        description: Some(description.to_string()),
        capabilities: ModelCapabilities {
            chat: true,
            embeddings: false,
            vision,
            tools,
        },
        source: ModelSource::Curated,
        featured,
        default_reasoning_effort: default_reasoning_effort.map(str::to_string),
        available_reasoning_efforts: available_reasoning_efforts
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

fn curated_embedding_model(
    provider: &str,
    id: &str,
    display_name: &str,
    description: &str,
) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        provider: provider.to_string(),
        description: Some(description.to_string()),
        capabilities: ModelCapabilities {
            chat: false,
            embeddings: true,
            vision: false,
            tools: false,
        },
        source: ModelSource::Curated,
        featured: false,
        default_reasoning_effort: None,
        available_reasoning_efforts: Vec::new(),
    }
}

fn catalog_rank(model: &ModelInfo) -> u8 {
    if model.featured {
        0
    } else if matches!(model.source, ModelSource::Curated | ModelSource::Manual) {
        1
    } else {
        2
    }
}

fn is_chat_compatible_id(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    [
        "embedding",
        "moderation",
        "audio",
        "babbage",
        "ada",
        "davinci",
        "curie",
        "cushman",
        "text-",
        "code-",
        "instruct",
        "whisper",
        "tts",
        "transcri",
        "dall-e",
        "image",
        "realtime",
    ]
    .iter()
    .all(|excluded| !id.contains(excluded))
}

fn is_embedding_id(id: &str) -> bool {
    id.to_ascii_lowercase().contains("embedding")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_marks_known_live_models_without_losing_curated_metadata() {
        let live = vec![ProviderModel {
            id: "gpt-5.6-sol".into(),
            owned_by: Some("openai".into()),
            kind: None,
        }];

        let catalog = merge_catalog("openai", "gpt-5.6-sol", &live);
        let model = catalog
            .iter()
            .find(|model| model.id == "gpt-5.6-sol")
            .unwrap();

        assert_eq!(model.display_name, "GPT-5.6 Sol");
        assert_eq!(model.source, ModelSource::Live);
        assert!(model.capabilities.chat);
    }

    #[test]
    fn merge_excludes_non_chat_models_and_keeps_unknown_chat_models() {
        let live = vec![
            ProviderModel {
                id: "text-embedding-3-small".into(),
                owned_by: None,
                kind: None,
            },
            ProviderModel {
                id: "text-davinci-003".into(),
                owned_by: None,
                kind: None,
            },
            ProviderModel {
                id: "gpt-3.5-turbo-instruct".into(),
                owned_by: None,
                kind: None,
            },
            ProviderModel {
                id: "text-ada-001".into(),
                owned_by: None,
                kind: None,
            },
            ProviderModel {
                id: "code-cushman-001".into(),
                owned_by: None,
                kind: None,
            },
            ProviderModel {
                id: "gpt-custom".into(),
                owned_by: Some("test".into()),
                kind: None,
            },
        ];

        let catalog = merge_catalog("openai", "gpt-custom", &live);

        assert!(!catalog
            .iter()
            .any(|model| model.id == "text-embedding-3-small"));
        assert!(!catalog.iter().any(|model| model.id == "text-davinci-003"));
        assert!(!catalog
            .iter()
            .any(|model| model.id == "gpt-3.5-turbo-instruct"));
        assert!(!catalog.iter().any(|model| model.id == "text-ada-001"));
        assert!(!catalog.iter().any(|model| model.id == "code-cushman-001"));
        assert_eq!(
            catalog
                .iter()
                .find(|model| model.id == "gpt-custom")
                .unwrap()
                .display_name,
            "gpt-custom"
        );
    }

    #[test]
    fn groq_catalog_comes_from_live_models_without_stale_curated_entries() {
        let live = vec![
            ProviderModel {
                id: "llama-3.3-70b-versatile".into(),
                owned_by: Some("groq".into()),
                kind: None,
            },
            ProviderModel {
                id: "whisper-large-v3".into(),
                owned_by: Some("groq".into()),
                kind: None,
            },
        ];

        let catalog = merge_catalog("groq", "", &live);

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id, "llama-3.3-70b-versatile");
        assert_eq!(catalog[0].source, ModelSource::Live);
    }

    #[test]
    fn explicit_provider_capabilities_allow_qdivzero_embedding_ids_without_heuristics() {
        let live = vec![ProviderModel {
            id: "qdiv-vector-v1".into(),
            owned_by: Some("qdivzero".into()),
            kind: Some(ProviderModelKind::Embedding),
        }];

        let catalog = merge_live_catalog("qdivzero", "", &live);
        assert_eq!(catalog.len(), 1);
        assert!(!catalog[0].capabilities.chat);
        assert!(catalog[0].capabilities.embeddings);
        assert!(merge_catalog("qdivzero", "", &live).is_empty());
    }

    #[test]
    fn merge_removes_duplicate_live_ids() {
        let live = vec![
            ProviderModel {
                id: "gpt-custom".into(),
                owned_by: None,
                kind: None,
            },
            ProviderModel {
                id: "gpt-custom".into(),
                owned_by: Some("duplicate".into()),
                kind: None,
            },
        ];

        let catalog = merge_catalog("openai", "gpt-5.6-sol", &live);

        assert_eq!(
            catalog
                .iter()
                .filter(|model| model.id == "gpt-custom")
                .count(),
            1
        );
    }

    #[test]
    fn search_matches_display_name_id_and_provider_case_insensitively() {
        let catalog = merge_catalog("openai", "gpt-5.6-sol", &[]);

        assert!(filter_models(&catalog, "5.6")
            .iter()
            .any(|model| model.id == "gpt-5.6-sol"));
        assert!(filter_models(&catalog, "openai")
            .iter()
            .all(|model| model.provider == "openai"));
        assert!(filter_models(&catalog, "not-present").is_empty());
    }

    #[test]
    fn response_parser_reads_ids_and_ownership() {
        let response = serde_json::json!({
            "data": [
                { "id": "gpt-5.6-sol", "owned_by": "openai" },
                { "id": "gpt-5.6-terra" }
            ]
        });

        assert_eq!(parse_provider_models(&response)[0].id, "gpt-5.6-sol");
        assert_eq!(
            parse_provider_models(&response)[0].owned_by.as_deref(),
            Some("openai")
        );
    }

    #[test]
    fn response_parser_keeps_usable_copilot_models_when_picker_flag_is_false() {
        let response = serde_json::json!({
            "data": [
                {
                    "id": "gpt-5.4",
                    "model_picker_enabled": true,
                    "supported_endpoints": ["/responses"]
                },
                {
                    "id": "hidden",
                    "model_picker_enabled": false,
                    "supported_endpoints": ["/chat/completions"]
                },
                {
                    "id": "messages-only",
                    "supported_endpoints": ["/v1/messages"]
                },
                {
                    "id": "blocked",
                    "policy": { "state": "disabled" },
                    "supported_endpoints": ["/chat/completions"]
                }
            ]
        });

        assert_eq!(
            parse_provider_models(&response)
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            vec!["gpt-5.4", "hidden"]
        );
    }
}
