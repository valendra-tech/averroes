#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub chat: bool,
    pub embeddings: bool,
    pub vision: bool,
    pub tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    Curated,
    Live,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub description: Option<String>,
    pub capabilities: ModelCapabilities,
    pub source: ModelSource,
    pub featured: bool,
    pub default_reasoning_effort: Option<String>,
    pub available_reasoning_efforts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModel {
    pub id: String,
    pub owned_by: Option<String>,
    /// Capability explicitly advertised by a provider catalog. `None` keeps
    /// compatibility with catalogs that expose no workload metadata and lets
    /// the model registry use its conservative ID heuristics.
    pub kind: Option<ProviderModelKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderModelKind {
    Chat,
    Embedding,
    Both,
}
