//! Model metadata, optional metadata overlays and provider catalog parsing.

mod catalog;
mod types;

pub use catalog::{
    curated_embedding_models, curated_models, filter_models, merge_catalog, merge_live_catalog,
    model_uses_reasoning_api, model_uses_responses_api, parse_provider_models, provider_label,
};
pub use types::{ModelCapabilities, ModelInfo, ModelSource, ProviderModel, ProviderModelKind};
