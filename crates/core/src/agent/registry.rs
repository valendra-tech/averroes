use super::Agent;
use dashmap::DashMap;
use std::sync::Arc;

pub struct AgentRegistry {
    agents: DashMap<String, Arc<Agent>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: DashMap::new(),
        }
    }

    pub fn register(&self, agent: Agent) {
        let id = agent.id().to_string();
        self.agents.insert(id, Arc::new(agent));
    }

    pub fn get(&self, id: &str) -> Option<Arc<Agent>> {
        self.agents.get(id).map(|r| r.clone())
    }

    pub fn list(&self) -> Vec<String> {
        self.agents.iter().map(|r| r.key().clone()).collect()
    }

    pub fn remove(&self, id: &str) -> Option<Arc<Agent>> {
        self.agents.remove(id).map(|(_, v)| v)
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::agent::AgentConfig;
    use crate::provider::types::{MessageContent, Role};
    use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider};
    use crate::runtime::ResourceGovernor;
    use crate::tool::ToolRegistry;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _r: ChatRequest) -> crate::provider::Result<ChatResponse> {
            Ok(ChatResponse {
                message: ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text("mock".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                usage: None,
                stop_reason: None,
            })
        }

        async fn chat_stream(
            &self,
            _r: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            unimplemented!()
        }

        fn context_window(&self, _m: &str) -> usize {
            200_000
        }

        fn supports_tools(&self, _m: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "mock"
        }
    }

    fn test_governor() -> Arc<ResourceGovernor> {
        Arc::new(ResourceGovernor::new(10, 200_000))
    }

    fn test_tool_registry() -> Arc<ToolRegistry> {
        Arc::new(ToolRegistry::new())
    }

    #[test]
    fn test_registry_new_empty() {
        let registry = AgentRegistry::new();
        assert!(registry.list().is_empty());
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let registry = AgentRegistry::new();
        let agent = Agent::new(
            AgentConfig::default(),
            Arc::new(MockProvider),
            test_tool_registry(),
            test_governor(),
            "session-reg".into(),
            PathBuf::from("/tmp"),
        );
        let id = agent.id().to_string();
        registry.register(agent);

        let found = registry.get(&id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id(), &id);
    }

    #[test]
    fn test_get_nonexistent() {
        let registry = AgentRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_list() {
        let registry = AgentRegistry::new();

        let agent1 = Agent::new(
            AgentConfig::default(),
            Arc::new(MockProvider),
            test_tool_registry(),
            test_governor(),
            "s1".into(),
            PathBuf::from("/tmp"),
        );
        let id1 = agent1.id().to_string();
        registry.register(agent1);

        let agent2 = Agent::new(
            AgentConfig::default(),
            Arc::new(MockProvider),
            test_tool_registry(),
            test_governor(),
            "s2".into(),
            PathBuf::from("/tmp"),
        );
        let id2 = agent2.id().to_string();
        registry.register(agent2);

        let list = registry.list();
        assert_eq!(list.len(), 2);
        assert!(list.contains(&id1));
        assert!(list.contains(&id2));
    }

    #[tokio::test]
    async fn test_remove() {
        let registry = AgentRegistry::new();
        let agent = Agent::new(
            AgentConfig::default(),
            Arc::new(MockProvider),
            test_tool_registry(),
            test_governor(),
            "session-rem".into(),
            PathBuf::from("/tmp"),
        );
        let id = agent.id().to_string();
        registry.register(agent);

        assert!(registry.get(&id).is_some());
        let removed = registry.remove(&id);
        assert!(removed.is_some());
        assert!(registry.get(&id).is_none());
    }

    #[test]
    fn test_default() {
        let registry = AgentRegistry::default();
        assert!(registry.list().is_empty());
    }
}
