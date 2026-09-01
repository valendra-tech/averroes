use crate::provider::{ModelCapabilities, ModelInfo, ModelSource};
use serde::{Deserialize, Serialize};

/// A model that can be declared in `~/.averroes/config/settings.toml` when a
/// provider does not expose a `/v1/models` endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub embeddings: bool,
    #[serde(default = "default_manual_tools")]
    pub tools: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub featured: bool,
}

const fn default_manual_tools() -> bool {
    true
}

impl ManualModel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            description: None,
            vision: false,
            embeddings: false,
            tools: true,
            default_reasoning_effort: None,
            reasoning_efforts: Vec::new(),
            featured: false,
        }
    }

    pub(super) fn into_info(self, provider: &str) -> Option<ModelInfo> {
        let id = self.id.trim().to_owned();
        if id.is_empty() {
            return None;
        }
        let embeddings = self.embeddings || looks_like_embedding_model(&id);

        Some(ModelInfo {
            display_name: self
                .display_name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| id.clone()),
            id,
            provider: provider.to_owned(),
            description: self.description.filter(|text| !text.trim().is_empty()),
            capabilities: ModelCapabilities {
                chat: !embeddings,
                embeddings,
                vision: self.vision,
                tools: self.tools,
            },
            source: ModelSource::Manual,
            featured: self.featured,
            default_reasoning_effort: self.default_reasoning_effort,
            available_reasoning_efforts: self.reasoning_efforts,
        })
    }
}

fn looks_like_embedding_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.contains("embed") || id.contains("embedding")
}
