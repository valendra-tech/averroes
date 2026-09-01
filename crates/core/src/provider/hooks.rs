//! Provider extension points.
//!
//! Adding a provider should be a bootstrap operation, not another match spread
//! across the runtime and the model picker. Hooks keep provider metadata and
//! discovery strategy in one place while the runtime remains responsible for
//! credentials and provider-specific clients.

use crate::connection::{ConnectionKind, ConnectionProfile};
use crate::models::ModelRegistry;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelDiscovery {
    /// A provider exposes an OpenAI/Anthropic/Ollama-compatible model list.
    RemoteApi,
    /// Models are discovered from the authenticated ChatGPT account.
    CodexAccount,
    /// Models are discovered from GitHub Copilot's authenticated catalog.
    CopilotAccount,
    /// The provider has no remote catalog; models must be declared manually.
    ManualOnly,
}

/// A lightweight provider registration. It deliberately does not own a
/// credential or an HTTP client; those stay in the runtime and are created
/// lazily only for an actual refresh or chat request.
pub trait ProviderHook: Send + Sync {
    fn id(&self) -> &'static str;
    fn kind(&self) -> ConnectionKind;
    fn catalog_provider(&self) -> Option<&'static str>;
    fn discovery(&self) -> ModelDiscovery;

    fn bootstrap_models(&self, profile: &ConnectionProfile, registry: &ModelRegistry) {
        registry.bootstrap_connection_with_manual_provider(
            profile,
            self.catalog_provider(),
            self.id(),
        );
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StandardProviderHook {
    id: &'static str,
    kind: ConnectionKind,
    catalog_provider: Option<&'static str>,
    discovery: ModelDiscovery,
}

impl StandardProviderHook {
    pub const fn new(
        id: &'static str,
        kind: ConnectionKind,
        catalog_provider: Option<&'static str>,
        discovery: ModelDiscovery,
    ) -> Self {
        Self {
            id,
            kind,
            catalog_provider,
            discovery,
        }
    }
}

impl ProviderHook for StandardProviderHook {
    fn id(&self) -> &'static str {
        self.id
    }

    fn kind(&self) -> ConnectionKind {
        self.kind
    }

    fn catalog_provider(&self) -> Option<&'static str> {
        self.catalog_provider
    }

    fn discovery(&self) -> ModelDiscovery {
        self.discovery
    }
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    hooks: HashMap<ConnectionKind, Arc<dyn ProviderHook>>,
}

impl ProviderRegistry {
    /// Builds the complete built-in provider set. Each provider module owns
    /// its registration function, so adding one does not require changing the
    /// model registry or UI code.
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        super::anthropic::register_provider_hook(&mut registry);
        super::codex::register_provider_hook(&mut registry);
        super::generic::register_provider_hooks(&mut registry);
        super::openai::register_provider_hook(&mut registry);
        super::qdivzero::register_provider_hook(&mut registry);
        registry.register(StandardProviderHook::new(
            "copilot",
            ConnectionKind::Copilot,
            None,
            ModelDiscovery::CopilotAccount,
        ));
        registry
    }

    pub fn register(&mut self, hook: impl ProviderHook + 'static) {
        self.hooks.insert(hook.kind(), Arc::new(hook));
    }

    pub fn hook(&self, kind: ConnectionKind) -> Option<Arc<dyn ProviderHook>> {
        self.hooks.get(&kind).cloned()
    }

    pub fn bootstrap(&self, profiles: &[ConnectionProfile], registry: &ModelRegistry) {
        for profile in profiles {
            if let Some(hook) = self.hook(profile.kind) {
                hook.bootstrap_models(profile, registry);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionProfile;

    #[test]
    fn builtins_register_every_connection_kind() {
        let providers = ProviderRegistry::with_builtins();
        assert_eq!(providers.len(), 10);
        assert!(providers.hook(ConnectionKind::Copilot).is_some());
        assert!(providers.hook(ConnectionKind::QDivZero).is_some());
        assert!(providers.hook(ConnectionKind::Groq).is_some());
        assert!(providers.hook(ConnectionKind::OllamaCloud).is_some());
        assert_eq!(
            providers.hook(ConnectionKind::Ollama).unwrap().discovery(),
            ModelDiscovery::RemoteApi
        );
    }

    #[test]
    fn bootstrap_is_provider_owned_and_does_not_require_credentials() {
        let providers = ProviderRegistry::with_builtins();
        let models = ModelRegistry::default();
        let profile = ConnectionProfile::copilot("copilot", "GitHub Copilot");

        providers.bootstrap(std::slice::from_ref(&profile), &models);
        assert_eq!(models.models(&profile.id).unwrap(), Vec::new());
    }
}
