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
    // ResourceGovernor stops its reset thread and Runtime shuts down after final Arc release.
    pub runtime: Arc<tokio::runtime::Runtime>,
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

        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(|error| RuntimeError::Configuration(error.to_string()))?,
        );

        let provider =
            create_provider(&config).map_err(|error| RuntimeError::Provider(error.to_string()))?;

        let tools = Arc::new(ToolRegistry::new());
        builtin::register_all(&tools);

        let (max_concurrent_calls, token_budget_per_minute) = runtime_limits(&config);
        let governor = Arc::new(ResourceGovernor::new(
            max_concurrent_calls,
            token_budget_per_minute,
        ));

        Ok(Self {
            config,
            provider,
            tools,
            governor,
            runtime,
        })
    }

    pub fn spawn_agent_run(
        &self,
        agent: Arc<Agent>,
        prompt: String,
    ) -> tokio::task::JoinHandle<anyhow::Result<String>> {
        self.runtime.spawn(async move { agent.run(&prompt).await })
    }

    pub fn new_agent(&self, session_id: &SessionId) -> Arc<Agent> {
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
            compaction: compaction_config(&self.config),
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

fn runtime_limits(config: &AppConfig) -> (usize, u64) {
    (
        config.runtime.max_concurrent_calls.unwrap_or(10).max(1),
        config
            .runtime
            .token_budget_per_minute
            .unwrap_or(200_000)
            .max(1),
    )
}

fn compaction_config(config: &AppConfig) -> CompactionConfig {
    CompactionConfig {
        strategy: match config.compaction.strategy.as_deref() {
            Some("trim") => CompactionStrategyType::Trim,
            Some("summary") => CompactionStrategyType::Summary,
            _ => CompactionStrategyType::Hybrid,
        },
        threshold: config.compaction.threshold.unwrap_or(0.8).clamp(0.0, 1.0),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use averroes_core::agent::AgentConfig;
    use averroes_core::config::AppConfig;
    use averroes_core::provider::{openai::OpenAiProvider, Provider};
    use averroes_core::runtime::ResourceGovernor;
    use averroes_core::tool::ToolRegistry;
    use std::sync::Arc;

    fn test_factory() -> AgentFactory {
        AgentFactory {
            config: AppConfig::default(),
            provider: Arc::new(OpenAiProvider::new("test-key".into())) as Arc<dyn Provider>,
            tools: Arc::new(ToolRegistry::new()),
            governor: Arc::new(ResourceGovernor::new(10, 200_000)),
            runtime: Arc::new(tokio::runtime::Runtime::new().unwrap()),
        }
    }

    #[test]
    fn new_agent_creates_a_fresh_agent_for_each_session() {
        let factory = test_factory();

        let mut sessions = SessionManager::new();
        let first_id = sessions.active().id.clone();
        let second_id = sessions.new_session();
        let first = factory.new_agent(&first_id);
        let second = factory.new_agent(&second_id);

        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn spawn_agent_run_uses_the_factory_runtime() {
        let factory = test_factory();
        let sessions = SessionManager::new();
        let agent = Arc::new(Agent::new(
            AgentConfig {
                max_iterations: 0,
                ..Default::default()
            },
            factory.provider.clone(),
            factory.tools.clone(),
            factory.governor.clone(),
            sessions.active().id.to_string(),
            ".".into(),
        ));

        let result = factory
            .runtime
            .block_on(factory.spawn_agent_run(agent, "hello".into()))
            .unwrap()
            .unwrap_err();

        assert!(result.to_string().contains("Max iterations"));
    }

    #[test]
    fn runtime_limits_have_safe_minimums() {
        let mut config = AppConfig::default();
        config.runtime.max_concurrent_calls = Some(0);
        config.runtime.token_budget_per_minute = Some(0);

        assert_eq!(runtime_limits(&config), (1, 1));
    }

    #[test]
    fn compaction_threshold_is_clamped() {
        let mut config = AppConfig::default();
        config.compaction.threshold = Some(2.0);
        assert_eq!(compaction_config(&config).threshold, 1.0);

        config.compaction.threshold = Some(-1.0);
        assert_eq!(compaction_config(&config).threshold, 0.0);
    }
}
