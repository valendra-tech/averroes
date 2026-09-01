use super::ManualModel;
use crate::connection::{ConnectionId, ConnectionProfile};
use crate::provider::{merge_live_catalog, ModelInfo, ModelSource, ProviderModel};
use dashmap::DashMap;
use std::sync::Arc;

/// Lock-free reads and atomic catalog replacement are important here: the
/// picker reads this data frequently while providers refresh over the network.
/// Each value is replaced as one `Arc`, so readers never observe a half-merged
/// catalog and no lock is held while a request is in flight.
#[derive(Clone, Default)]
pub struct ModelRegistry {
    catalogs: Arc<DashMap<ConnectionId, Arc<Vec<ModelInfo>>>>,
}

impl ModelRegistry {
    /// Installs the initial catalog for a connection during provider bootstrap.
    pub fn bootstrap_connection(
        &self,
        profile: &ConnectionProfile,
        curated_provider: Option<&str>,
    ) -> Vec<ModelInfo> {
        self.bootstrap_connection_with_manual_provider(
            profile,
            curated_provider,
            curated_provider.unwrap_or("generic"),
        )
    }

    pub fn bootstrap_connection_with_manual_provider(
        &self,
        profile: &ConnectionProfile,
        _catalog_provider: Option<&str>,
        manual_provider: &str,
    ) -> Vec<ModelInfo> {
        // A connection starts empty. Only a live provider response or an
        // explicitly configured manual model may populate its catalog.
        let mut models = Vec::new();
        apply_manual_models(&mut models, manual_provider, &profile.manual_models);
        sort_models(&mut models);
        self.replace(profile.id.clone(), models.clone());
        models
    }

    /// Replaces a connection catalog with the provider's live `/models`
    /// response and the models explicitly configured by the user.
    pub fn register_live(
        &self,
        profile: &ConnectionProfile,
        provider: &str,
        live: &[ProviderModel],
    ) -> Vec<ModelInfo> {
        let mut models = merge_live_catalog(provider, "", live);
        apply_manual_models(&mut models, provider, &profile.manual_models);
        sort_models(&mut models);
        self.replace(profile.id.clone(), models.clone());
        models
    }

    /// Replaces a catalog produced by a provider with richer provider-specific
    /// metadata (for example Copilot's endpoint and reasoning capabilities).
    pub fn replace_provider_models(
        &self,
        profile: &ConnectionProfile,
        provider: &str,
        mut models: Vec<ModelInfo>,
    ) -> Vec<ModelInfo> {
        apply_manual_models(&mut models, provider, &profile.manual_models);
        sort_models(&mut models);
        self.replace(profile.id.clone(), models.clone());
        models
    }

    /// Registers one manual model immediately. This is useful for settings
    /// screens and integrations that do not want to rewrite the full profile.
    pub fn register_manual_model(
        &self,
        connection_id: &ConnectionId,
        provider: &str,
        model: ManualModel,
    ) -> bool {
        let Some(model) = model.into_info(provider) else {
            return false;
        };
        let mut models = self.models(connection_id).unwrap_or_default();
        upsert_model(&mut models, model);
        sort_models(&mut models);
        self.replace(connection_id.clone(), models);
        true
    }

    pub fn models(&self, connection_id: &ConnectionId) -> Option<Vec<ModelInfo>> {
        self.catalogs
            .get(connection_id)
            .map(|catalog| catalog.value().as_ref().clone())
    }

    pub fn replace(&self, connection_id: ConnectionId, models: Vec<ModelInfo>) {
        self.catalogs.insert(connection_id, Arc::new(models));
    }

    pub fn remove(&self, connection_id: &ConnectionId) {
        self.catalogs.remove(connection_id);
    }

    pub fn clear(&self) {
        self.catalogs.clear();
    }
}

fn apply_manual_models(models: &mut Vec<ModelInfo>, provider: &str, manual: &[ManualModel]) {
    for model in manual.iter().cloned() {
        if let Some(model) = model.into_info(provider) {
            upsert_model(models, model);
        }
    }
}

fn upsert_model(models: &mut Vec<ModelInfo>, model: ModelInfo) {
    if let Some(existing) = models.iter_mut().find(|existing| existing.id == model.id) {
        *existing = model;
    } else {
        models.push(model);
    }
}

fn sort_models(models: &mut [ModelInfo]) {
    models.sort_by(|left, right| {
        model_rank(left)
            .cmp(&model_rank(right))
            .then_with(|| {
                left.display_name
                    .to_ascii_lowercase()
                    .cmp(&right.display_name.to_ascii_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn model_rank(model: &ModelInfo) -> u8 {
    if model.featured {
        0
    } else {
        match model.source {
            ModelSource::Curated | ModelSource::Manual => 1,
            ModelSource::Live => 2,
        }
    }
}
