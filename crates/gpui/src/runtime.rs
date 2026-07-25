use crate::session::SessionId;
use averroes_core::agent::{Agent, AgentConfig};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::config::{create_provider, AppConfig};
use averroes_core::provider::Provider;
use averroes_core::runtime::ResourceGovernor;
use averroes_core::tool::{builtin, ToolRegistry};
use std::sync::Arc;

pub struct AgentFactory {
    pub config: AppConfig,
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub governor: Arc<ResourceGovernor>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("setup required")]
    NeedsSetup,
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("provider error: {0}")]
    Provider(String),
}

impl AgentFactory {
    pub fn load() -> Result<Self, RuntimeError> {
        let config =
            AppConfig::load().map_err(|error| RuntimeError::Configuration(error.to_string()))?;

        if config.needs_setup() {
            return Err(RuntimeError::NeedsSetup);
        }

        let provider =
            create_provider(&config).map_err(|error| RuntimeError::Provider(error.to_string()))?;

        let tools = Arc::new(ToolRegistry::new());
        builtin::register_all(&tools);

        let governor = Arc::new(ResourceGovernor::new(
            config.runtime.max_concurrent_calls.unwrap_or(10),
            config.runtime.token_budget_per_minute.unwrap_or(200_000),
        ));

        Ok(Self {
            config,
            provider,
            tools,
            governor,
        })
    }

    pub fn new_agent(&self, session_id: &SessionId) -> Arc<Agent> {
        let compaction = CompactionConfig {
            strategy: match self.config.compaction.strategy.as_deref() {
                Some("trim") => CompactionStrategyType::Trim,
                Some("summary") => CompactionStrategyType::Summary,
                _ => CompactionStrategyType::Hybrid,
            },
            threshold: self.config.compaction.threshold.unwrap_or(0.8),
            ..Default::default()
        };

        let agent_config = AgentConfig {
            name: "gpui".into(),
            model: self.provider.default_model().to_string(),
            tools: vec![
                "bash".into(),
                "file_read".into(),
                "file_write".into(),
                "glob".into(),
                "grep".into(),
                "web_fetch".into(),
            ],
            max_iterations: 30,
            compaction,
            ..Default::default()
        };

        Arc::new(Agent::new(
            agent_config,
            self.provider.clone(),
            self.tools.clone(),
            self.governor.clone(),
            session_id.to_string(),
            std::env::current_dir().unwrap_or_else(|_| ".".into()),
        ))
    }

    pub fn reload() -> Result<Self, RuntimeError> {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use averroes_core::config::AppConfig;
    use averroes_core::provider::{openai::OpenAiProvider, Provider};
    use averroes_core::runtime::ResourceGovernor;
    use averroes_core::tool::ToolRegistry;
    use std::sync::Arc;

    #[test]
    fn new_agent_creates_a_fresh_agent_for_each_session() {
        let factory = AgentFactory {
            config: AppConfig::default(),
            provider: Arc::new(OpenAiProvider::new("test-key".into())) as Arc<dyn Provider>,
            tools: Arc::new(ToolRegistry::new()),
            governor: Arc::new(ResourceGovernor::new(10, 200_000)),
        };

        let mut sessions = SessionManager::new();
        let first_id = sessions.active().id.clone();
        let second_id = sessions.new_session();
        let first = factory.new_agent(&first_id);
        let second = factory.new_agent(&second_id);

        assert_ne!(first.id(), second.id());
    }
}
