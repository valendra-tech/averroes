//! Central model domain.
//!
//! `manual` describes user-owned model metadata, `registry` owns live
//! connection catalogs, and `provider/models` contains provider-specific
//! parsing and optional metadata overlays.

mod manual;
mod registry;

pub use manual::ManualModel;
pub use registry::ModelRegistry;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionKind, ConnectionProfile};
    use crate::provider::{ModelSource, ProviderModel};

    #[test]
    fn manual_models_are_persistable_and_override_curated_metadata() {
        let mut profile = ConnectionProfile::api(
            "local",
            "Local",
            ConnectionKind::Compatible,
            Some("https://example.test/v1".into()),
        );
        profile.manual_models.push(ManualModel {
            id: "custom-model".into(),
            display_name: Some("My private model".into()),
            description: Some("Declared locally".into()),
            vision: true,
            embeddings: false,
            tools: false,
            default_reasoning_effort: None,
            reasoning_efforts: vec![],
            featured: true,
        });

        let registry = ModelRegistry::default();
        let models = registry.bootstrap_connection(&profile, Some("openai"));
        let custom = models
            .iter()
            .find(|model| model.id == "custom-model")
            .unwrap();

        assert_eq!(custom.display_name, "My private model");
        assert_eq!(custom.source, ModelSource::Manual);
        assert!(custom.capabilities.vision);
        assert!(!custom.capabilities.tools);
    }

    #[test]
    fn replacing_a_catalog_is_atomic_from_the_readers_perspective() {
        let profile = ConnectionProfile::ollama("ollama", "Ollama", None);
        let registry = ModelRegistry::default();
        assert!(registry.models(&profile.id).is_none());

        registry.bootstrap_connection(&profile, None);
        assert_eq!(registry.models(&profile.id).unwrap().len(), 0);

        assert!(registry.register_manual_model(&profile.id, "ollama", ManualModel::new("qwen3")));
        assert_eq!(registry.models(&profile.id).unwrap()[0].id, "qwen3");
    }

    #[test]
    fn bootstrap_does_not_add_provider_default_models() {
        let profile = ConnectionProfile::api(
            "openai",
            "OpenAI API",
            ConnectionKind::OpenAi,
            Some("https://api.openai.com/v1".into()),
        );
        let registry = ModelRegistry::default();
        registry.bootstrap_connection(&profile, Some("openai"));

        let models = registry.models(&profile.id).unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn live_embedding_models_are_registered_only_when_advertised() {
        let profile = ConnectionProfile::api(
            "openai",
            "OpenAI API",
            ConnectionKind::OpenAi,
            Some("https://api.openai.com/v1".into()),
        );
        let registry = ModelRegistry::default();
        registry.bootstrap_connection(&profile, Some("openai"));
        assert!(registry.models(&profile.id).unwrap().is_empty());

        let models = registry.register_live(
            &profile,
            "openai",
            &[ProviderModel {
                id: "text-embedding-3-small".into(),
                owned_by: Some("openai".into()),
                kind: None,
            }],
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].source, ModelSource::Live);
        assert!(models[0].capabilities.embeddings);
        assert!(!models[0].capabilities.chat);
    }
}
