pub mod orchestration;
pub mod registry;

use crate::compaction::{CompactionConfig, CompactionStrategy, CompactionStrategyType};
use crate::compaction::strategies::{TrimStrategy, SummaryStrategy, HybridStrategy};
use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolDefinition};
use crate::provider::types::{ContentPart, MessageContent, Role};
use crate::runtime::ResourceGovernor;
use crate::tool::{ToolContext, ToolRegistry, ToolResult};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub max_iterations: usize,
    pub compaction: CompactionConfig,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".into(),
            model: "claude-sonnet-4-20250514".into(),
            system_prompt: None,
            tools: Vec::new(),
            max_iterations: 50,
            compaction: CompactionConfig::default(),
            temperature: None,
            max_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Acting,
    WaitingForTool,
    Compacting,
    Completed,
    Errored,
    Cancelled,
}

pub struct Agent {
    config: AgentConfig,
    provider: Arc<dyn Provider>,
    tool_registry: Arc<ToolRegistry>,
    governor: Arc<ResourceGovernor>,
    state: Arc<Mutex<AgentState>>,
    messages: Arc<tokio::sync::Mutex<Vec<ChatMessage>>>,
    agent_id: String,
    session_id: String,
    working_dir: PathBuf,
}

fn estimate_request_tokens(request: &ChatRequest) -> u64 {
    let message_chars = request.messages.iter().fold(0u64, |total, message| {
        let chars = match &message.content {
            MessageContent::Text(text) => text.chars().count() as u64,
            MessageContent::Parts(parts) => parts.iter().fold(0u64, |total, part| {
                let chars = match part {
                    ContentPart::Text { text } => text.chars().count() as u64,
                    ContentPart::ToolResult { content, .. } => content.chars().count() as u64,
                    _ => 0,
                };
                total.saturating_add(chars)
            }),
        };
        total.saturating_add(chars)
    });

    (message_chars / 4).saturating_add(request.max_tokens.unwrap_or(4096) as u64)
}

impl Agent {
    pub fn new(
        config: AgentConfig,
        provider: Arc<dyn Provider>,
        tool_registry: Arc<ToolRegistry>,
        governor: Arc<ResourceGovernor>,
        session_id: String,
        working_dir: PathBuf,
    ) -> Self {
        let agent_id = uuid::Uuid::new_v4().to_string();
        let messages = {
            let mut msgs = Vec::new();
            if let Some(ref prompt) = config.system_prompt {
                msgs.push(ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text(prompt.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            Arc::new(tokio::sync::Mutex::new(msgs))
        };

        Self {
            config,
            provider,
            tool_registry,
            governor,
            state: Arc::new(Mutex::new(AgentState::Idle)),
            messages,
            agent_id,
            session_id,
            working_dir,
        }
    }

    pub fn id(&self) -> &str {
        &self.agent_id
    }

    pub async fn state(&self) -> AgentState {
        *self.state.lock().await
    }

    pub async fn run(&self, user_input: &str) -> Result<String> {
        {
            let mut msgs = self.messages.lock().await;
            msgs.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_input.to_string()),
                tool_call_id: None,
                tool_calls: None,
            });
        }

        for _iteration in 0..self.config.max_iterations {
            if self.should_compact().await {
                self.compact().await?;
            }

            let _permit = self.governor.acquire_call_permit().await;

            let tool_defs = self.build_tool_definitions();

            let request = {
                let msgs = self.messages.lock().await;
                ChatRequest {
                    model: self.config.model.clone(),
                    messages: msgs.clone(),
                    tools: tool_defs,
                    max_tokens: self.config.max_tokens,
                    temperature: self.config.temperature,
                    system: None,
                }
            };

            self.set_state(AgentState::Thinking).await;

            let reserved_tokens = estimate_request_tokens(&request);
            if !self.governor.try_reserve_tokens(reserved_tokens) {
                self.set_state(AgentState::Errored).await;
                return Err(anyhow::anyhow!(
                    "Token budget exhausted before provider request"
                ));
            }

            let response = match self.provider.chat(request).await {
                Ok(response) => response,
                Err(error) => {
                    self.governor.release_tokens(reserved_tokens);
                    return Err(anyhow::anyhow!(error.to_string()));
                }
            };

            if let Some(usage) = response.usage.as_ref() {
                let actual_tokens = usage
                    .input_tokens
                    .saturating_add(usage.output_tokens);
                if actual_tokens < reserved_tokens {
                    self.governor
                        .release_tokens(reserved_tokens - actual_tokens);
                } else if actual_tokens > reserved_tokens {
                    let extra_tokens = actual_tokens - reserved_tokens;
                    if !self.governor.try_reserve_tokens(extra_tokens) {
                        self.governor.release_tokens(reserved_tokens);
                        self.set_state(AgentState::Errored).await;
                        return Err(anyhow::anyhow!(
                            "Token budget exhausted after provider response"
                        ));
                    }
                }
            }

            if response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()) {
                self.set_state(AgentState::Acting).await;

                {
                    let mut msgs = self.messages.lock().await;
                    msgs.push(ChatMessage {
                        role: Role::Assistant,
                        content: MessageContent::Text(String::new()),
                        tool_call_id: None,
                        tool_calls: response.message.tool_calls.clone(),
                    });
                }

                let tool_messages = self.execute_tools(&response).await?;
                {
                    let mut msgs = self.messages.lock().await;
                    msgs.extend(tool_messages);
                }

                continue;
            }

            self.set_state(AgentState::Completed).await;
            return Ok(match &response.message.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Parts(_) => String::from("response with parts"),
            });
        }

        Err(anyhow::anyhow!("Max iterations reached"))
    }

    async fn execute_tools(&self, response: &ChatResponse) -> Result<Vec<ChatMessage>> {
        let tool_calls = match &response.message.tool_calls {
            Some(tc) => tc,
            None => return Ok(Vec::new()),
        };

        let ctx = ToolContext {
            working_dir: self.working_dir.clone(),
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
        };

        let mut messages = Vec::new();

        for tc in tool_calls {
            let params: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(e) => {
                    messages.push(ChatMessage {
                        role: Role::Tool,
                        content: MessageContent::Text(format!("invalid arguments: {}", e)),
                        tool_call_id: Some(tc.id.clone()),
                        tool_calls: None,
                    });
                    continue;
                }
            };

            let result = match self.tool_registry.execute(&tc.function.name, &ctx, &params).await {
                Ok(r) => r,
                Err(e) => ToolResult::error(e.to_string()),
            };

            let content = if result.success {
                result.content
            } else {
                result.error.unwrap_or_else(|| String::from("unknown error"))
            };

            messages.push(ChatMessage {
                role: Role::Tool,
                content: MessageContent::Text(content),
                tool_call_id: Some(tc.id.clone()),
                tool_calls: None,
            });
        }

        Ok(messages)
    }

    async fn should_compact(&self) -> bool {
        let context_limit = self.provider.context_window(&self.config.model);
        let strategy = self.compaction_strategy();
        let msgs = self.messages.lock().await;
        strategy.should_compact(&msgs, context_limit, &self.config.compaction)
    }

    async fn compact(&self) -> Result<()> {
        self.set_state(AgentState::Compacting).await;

        let strategy = self.compaction_strategy();
        let context_limit = self.provider.context_window(&self.config.model);
        let provider_ref: Option<&dyn Provider> = Some(self.provider.as_ref());

        let compacted = {
            let msgs = self.messages.lock().await;
            strategy
                .compact(&msgs, context_limit, &self.config.compaction, provider_ref)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
        };

        {
            let mut msgs = self.messages.lock().await;
            *msgs = compacted.messages;
        }
        Ok(())
    }

    fn compaction_strategy(&self) -> Box<dyn CompactionStrategy> {
        match self.config.compaction.strategy {
            CompactionStrategyType::Trim => Box::new(TrimStrategy),
            CompactionStrategyType::Summary => Box::new(SummaryStrategy),
            CompactionStrategyType::Hybrid => Box::new(HybridStrategy),
        }
    }

    async fn set_state(&self, state: AgentState) {
        *self.state.lock().await = state;
    }

    fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.config
            .tools
            .iter()
            .filter_map(|name| {
                self.tool_registry.get(name).map(|tool| ToolDefinition {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.parameters(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FunctionCall, Role as ProviderRole, TokenUsage, ToolCall};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct TestProvider {
        responses: Vec<ChatResponse>,
        call_count: std::sync::Mutex<usize>,
    }

    impl TestProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses,
                call_count: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Provider for TestProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            let mut count = self.call_count.lock().unwrap();
            let idx = *count;
            *count += 1;
            Ok(self.responses.get(idx).cloned().unwrap_or(ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("fallback".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                stop_reason: None,
            }))
        }

        async fn chat_stream(&self, _r: ChatRequest) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test-model"
        }
    }

    fn test_agent_config() -> AgentConfig {
        AgentConfig {
            name: "test-agent".into(),
            model: "test-model".into(),
            system_prompt: Some("You are a test agent.".into()),
            tools: vec!["echo".into()],
            ..Default::default()
        }
    }

    fn test_tool_registry() -> Arc<ToolRegistry> {
        use crate::tool::Tool;
        use async_trait::async_trait;
        use serde_json::json;

        struct EchoTool;
        #[async_trait]
        impl Tool for EchoTool {
            fn name(&self) -> &str { "echo" }
            fn description(&self) -> &str { "Echoes input" }
            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {"text": {"type": "string"}}})
            }
            async fn execute(&self, _ctx: &ToolContext, params: &serde_json::Value) -> crate::tool::Result<ToolResult> {
                Ok(ToolResult::ok(params["text"].as_str().unwrap_or("")))
            }
        }

        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        Arc::new(registry)
    }

    fn test_governor() -> Arc<ResourceGovernor> {
        Arc::new(ResourceGovernor::new(10, 200_000))
    }

    #[test]
    fn test_agent_config_defaults() {
        let config = AgentConfig::default();
        assert_eq!(config.name, "default");
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.max_iterations, 50);
    }

    #[test]
    fn test_agent_config_custom() {
        let config = AgentConfig {
            name: "custom".into(),
            model: "gpt-5".into(),
            max_iterations: 10,
            temperature: Some(0.5),
            max_tokens: Some(4096),
            ..Default::default()
        };
        assert_eq!(config.name, "custom");
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.max_tokens, Some(4096));
    }

    #[test]
    fn test_agent_state_copy() {
        let state = AgentState::Idle;
        assert_eq!(state, AgentState::Idle);
        let copy = state;
        assert_eq!(copy, AgentState::Idle);
        assert_ne!(state, AgentState::Completed);
    }

    #[tokio::test]
    async fn test_agent_new() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "session-1".into(),
            PathBuf::from("/tmp"),
        );
        assert!(!agent.id().is_empty());
    }

    #[tokio::test]
    async fn test_agent_state_transitions() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "session-1".into(),
            PathBuf::from("/tmp"),
        );
        assert_eq!(agent.state().await, AgentState::Idle);
    }

    #[tokio::test]
    async fn test_agent_run_simple_response() {
        let provider = Arc::new(TestProvider::new(vec![
            ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("Hello, world!".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                stop_reason: None,
            },
        ]));

        let agent = Agent::new(
            test_agent_config(),
            provider,
            test_tool_registry(),
            test_governor(),
            "session-1".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("hi").await.unwrap();
        assert_eq!(result, "Hello, world!");
        assert_eq!(agent.state().await, AgentState::Completed);
    }

    #[tokio::test]
    async fn test_agent_rejects_request_before_provider_when_reservation_exceeds_budget() {
        let provider = Arc::new(TestProvider::new(vec![]));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                max_tokens: Some(1),
                ..Default::default()
            },
            provider.clone(),
            test_tool_registry(),
            Arc::new(ResourceGovernor::new(1, 1)),
            "reservation-session".into(),
            PathBuf::from("/tmp"),
        );

        let error = agent.run("request").await.unwrap_err();

        assert!(error
            .to_string()
            .contains("Token budget exhausted before provider request"));
        assert_eq!(*provider.call_count.lock().unwrap(), 0);
        assert_eq!(agent.state().await, AgentState::Errored);
    }

    #[tokio::test]
    async fn test_agent_rejects_response_when_token_budget_is_exhausted() {
        let response = ChatResponse {
            message: ChatMessage {
                role: ProviderRole::Assistant,
                content: MessageContent::Text("budgeted response".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            usage: Some(TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
            stop_reason: None,
        };
        let provider = Arc::new(TestProvider::new(vec![response.clone(), response]));
        let governor = Arc::new(ResourceGovernor::new(1, 5));
        let agent = Agent::new(
            AgentConfig {
                system_prompt: None,
                tools: Vec::new(),
                max_tokens: Some(4),
                ..Default::default()
            },
            provider,
            test_tool_registry(),
            governor.clone(),
            "token-budget-session".into(),
            PathBuf::from("/tmp"),
        );

        assert_eq!(agent.run("first").await.unwrap(), "budgeted response");
        assert!(agent.run("second").await.is_err());
        assert_eq!(agent.state().await, AgentState::Errored);
        assert_eq!(governor.tokens_available(), 0);
    }

    #[tokio::test]
    async fn test_agent_run_with_tool_calls() {
        let provider = Arc::new(TestProvider::new(vec![
            ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text(String::new()),
                    tool_call_id: None,
                    tool_calls: Some(vec![
                        ToolCall {
                            id: "tc_1".into(),
                            call_type: "function".into(),
                            function: FunctionCall {
                                name: "echo".into(),
                                arguments: r#"{"text": "hello from tool"}"#.into(),
                            },
                        },
                    ]),
                },
                usage: None,
                stop_reason: None,
            },
            ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("Done after tool.".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                stop_reason: None,
            },
        ]));

        let agent = Agent::new(
            test_agent_config(),
            provider,
            test_tool_registry(),
            test_governor(),
            "session-2".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("do something").await.unwrap();
        assert_eq!(result, "Done after tool.");
        assert_eq!(agent.state().await, AgentState::Completed);
    }

    #[tokio::test]
    async fn test_agent_run_max_iterations() {
        let provider = Arc::new(TestProvider::new({
            let mut responses = Vec::new();
            for _ in 0..3 {
                responses.push(ChatResponse {
                    message: ChatMessage {
                        role: ProviderRole::Assistant,
                        content: MessageContent::Text(String::new()),
                        tool_call_id: None,
                        tool_calls: Some(vec![
                            ToolCall {
                                id: "tc_loop".into(),
                                call_type: "function".into(),
                                function: FunctionCall {
                                    name: "echo".into(),
                                    arguments: r#"{"text": "loop"}"#.into(),
                                },
                            },
                        ]),
                    },
                    usage: None,
                    stop_reason: None,
                });
            }
            responses
        }));

        let agent = Agent::new(
            AgentConfig {
                max_iterations: 2,
                ..test_agent_config()
            },
            provider,
            test_tool_registry(),
            test_governor(),
            "session-3".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("loop forever").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Max iterations"));
    }

    #[tokio::test]
    async fn test_agent_builds_tool_definitions() {
        let agent = Agent::new(
            test_agent_config(),
            Arc::new(TestProvider::new(vec![])),
            test_tool_registry(),
            test_governor(),
            "session-4".into(),
            PathBuf::from("/tmp"),
        );

        let defs = agent.build_tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[tokio::test]
    async fn test_agent_system_prompt() {
        let config = AgentConfig {
            system_prompt: Some("You are helpful.".into()),
            tools: vec![],
            ..test_agent_config()
        };

        let provider = Arc::new(TestProvider::new(vec![
            ChatResponse {
                message: ChatMessage {
                    role: ProviderRole::Assistant,
                    content: MessageContent::Text("ok".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                stop_reason: None,
            },
        ]));

        let agent = Agent::new(
            config,
            provider,
            test_tool_registry(),
            test_governor(),
            "session-5".into(),
            PathBuf::from("/tmp"),
        );

        let result = agent.run("test").await.unwrap();
        assert_eq!(result, "ok");
    }
}
