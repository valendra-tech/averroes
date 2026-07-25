use crate::session::SessionId;
use averroes_core::agent::{Agent, AgentConfig};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::config::{create_provider, AppConfig, ConfigError};
use averroes_core::provider::Provider;
use averroes_core::runtime::ResourceGovernor;
use averroes_core::tool::{builtin, ToolRegistry};
use std::sync::Arc;

const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.8;
const MAX_CONCURRENT_CALLS: usize = tokio::sync::Semaphore::MAX_PERMITS;

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
    #[error("provider error: {message}")]
    Provider {
        message: String,
        api_key_env: Option<String>,
    },
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

        let provider = match create_provider(&config) {
            Ok(provider) => provider,
            Err(error) => return Err(provider_runtime_error(error)),
        };

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

fn provider_runtime_error(error: ConfigError) -> RuntimeError {
    let message = error.to_string();
    match error {
        ConfigError::MissingApiKey { api_key_env } => RuntimeError::Provider {
            message,
            api_key_env: Some(api_key_env),
        },
        _ => RuntimeError::Configuration(message),
    }
}

fn runtime_limits(config: &AppConfig) -> (usize, u64) {
    (
        config
            .runtime
            .max_concurrent_calls
            .unwrap_or(10)
            .clamp(1, MAX_CONCURRENT_CALLS),
        config
            .runtime
            .token_budget_per_minute
            .unwrap_or(200_000)
            .max(1),
    )
}

fn compaction_config(config: &AppConfig) -> CompactionConfig {
    let threshold = config
        .compaction
        .threshold
        .unwrap_or(DEFAULT_COMPACTION_THRESHOLD);

    CompactionConfig {
        strategy: match config.compaction.strategy.as_deref() {
            Some("trim") => CompactionStrategyType::Trim,
            Some("summary") => CompactionStrategyType::Summary,
            _ => CompactionStrategyType::Hybrid,
        },
        threshold: if threshold.is_finite() {
            threshold.clamp(0.0, 1.0)
        } else {
            DEFAULT_COMPACTION_THRESHOLD
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use averroes_core::agent::AgentConfig;
    use averroes_core::config::AppConfig;
    use averroes_core::provider::types::{MessageContent, Role};
    use averroes_core::provider::{
        openai::OpenAiProvider, ChatMessage, ChatRequest, ChatResponse, ChatStream, Provider,
        ProviderError,
    };
    use averroes_core::runtime::ResourceGovernor;
    use averroes_core::tool::ToolRegistry;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(
            &self,
            _request: ChatRequest,
        ) -> averroes_core::provider::Result<ChatResponse> {
            Ok(ChatResponse {
                message: ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text("mock response".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                stop_reason: None,
            })
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> averroes_core::provider::Result<ChatStream> {
            Err(ProviderError::Other("stream unused in test".into()))
        }

        fn context_window(&self, _model: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _model: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "mock-model"
        }
    }

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
    fn spawn_agent_run_returns_mock_provider_response() {
        let factory = AgentFactory {
            config: AppConfig::default(),
            provider: Arc::new(MockProvider),
            tools: Arc::new(ToolRegistry::new()),
            governor: Arc::new(ResourceGovernor::new(10, 200_000)),
            runtime: Arc::new(tokio::runtime::Runtime::new().unwrap()),
        };
        let sessions = SessionManager::new();
        let agent = factory.new_agent(&sessions.active().id);

        let result = factory
            .runtime
            .block_on(factory.spawn_agent_run(agent, "hello".into()))
            .unwrap()
            .unwrap();

        assert_eq!(result, "mock response");
    }

    #[test]
    fn runtime_limits_have_safe_minimums() {
        let mut config = AppConfig::default();
        config.runtime.max_concurrent_calls = Some(0);
        config.runtime.token_budget_per_minute = Some(0);

        assert_eq!(runtime_limits(&config), (1, 1));
    }

    #[test]
    fn runtime_limits_cap_concurrent_calls_at_tokio_maximum() {
        let mut config = AppConfig::default();
        config.runtime.max_concurrent_calls = Some(usize::MAX);

        assert_eq!(
            runtime_limits(&config).0,
            tokio::sync::Semaphore::MAX_PERMITS
        );
    }

    #[test]
    fn compaction_threshold_is_clamped() {
        let mut config = AppConfig::default();
        config.compaction.threshold = Some(2.0);
        assert_eq!(compaction_config(&config).threshold, 1.0);

        config.compaction.threshold = Some(-1.0);
        assert_eq!(compaction_config(&config).threshold, 0.0);
    }

    #[test]
    fn compaction_threshold_uses_default_for_non_finite_values() {
        let mut config = AppConfig::default();
        config.compaction.threshold = Some(f64::NAN);
        assert_eq!(compaction_config(&config).threshold, 0.8);

        config.compaction.threshold = Some(f64::INFINITY);
        assert_eq!(compaction_config(&config).threshold, 0.8);

        config.compaction.threshold = Some(f64::NEG_INFINITY);
        assert_eq!(compaction_config(&config).threshold, 0.8);
    }
}
