use super::{EnabledTool, Result, Tool, ToolContext, ToolError, ToolRef, ToolResult};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;

pub struct ToolRegistry {
    tools: DashMap<String, ToolRef>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: DashMap::new(),
        }
    }

    /// Register a tool with the registry. If a tool with the same name already
    /// exists, it is overwritten.
    pub fn register(&self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<ToolRef> {
        if let Some(tool) = self.tools.get(name) {
            return Some(tool.clone());
        }
        if name == "call_agent" {
            return self.tools.get("call_agents").map(|tool| tool.clone());
        }
        None
    }

    pub fn list(&self) -> Vec<ToolRef> {
        self.tools.iter().map(|r| r.clone()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.catalog().into_iter().map(|tool| tool.name).collect()
    }

    /// Returns a stable, compact view of every scoped tool. Descriptions are
    /// suitable for discovery; full parameter schemas remain lazy.
    pub fn catalog(&self) -> Vec<EnabledTool> {
        let mut catalog = self
            .tools
            .iter()
            .map(|entry| EnabledTool {
                name: entry.key().clone(),
                description: entry.value().description().to_string(),
            })
            .collect::<Vec<_>>();
        catalog.sort_by(|left, right| left.name.cmp(&right.name));
        catalog
    }

    /// Names of the minimal tools that seed a new conversation before it
    /// discovers and activates more specific capabilities.
    pub fn bootstrap_names(&self) -> Vec<String> {
        let mut names = self
            .tools
            .iter()
            .filter(|entry| entry.value().is_bootstrap())
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Create an isolated registry that shares the existing tool implementations.
    ///
    /// This lets a workspace add scoped tools without mutating the application's
    /// global registry or leaking them into agents from another workspace.
    pub fn fork(&self) -> Self {
        let registry = Self::new();
        for entry in &self.tools {
            registry
                .tools
                .insert(entry.key().clone(), entry.value().clone());
        }
        registry
    }

    pub fn remove(&self, name: &str) -> Option<ToolRef> {
        self.tools.remove(name).map(|(_, v)| v)
    }

    pub async fn execute(
        &self,
        name: &str,
        ctx: &ToolContext,
        params: &Value,
    ) -> Result<ToolResult> {
        let tool = self.get(name).ok_or_else(|| ToolError::NotFound {
            tool: name.to_string(),
        })?;
        tool.execute(ctx, params).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Result, ToolContext, ToolResult};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::PathBuf;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echoes back the input"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to echo back"
                    }
                },
                "required": ["text"]
            })
        }

        async fn execute(
            &self,
            _ctx: &ToolContext,
            params: &serde_json::Value,
        ) -> Result<ToolResult> {
            let text = params["text"].as_str().unwrap_or("");
            Ok(ToolResult::ok(text))
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "test-session".into(),
            agent_id: "test-agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(super::super::ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[test]
    fn test_register_and_get() {
        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        let tool = registry.get("echo");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "echo");
    }

    #[tokio::test]
    async fn test_execute() {
        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        let result = registry
            .execute("echo", &test_ctx(), &json!({"text": "hello"}))
            .await;
        assert!(result.is_ok());
        let tool_result = result.unwrap();
        assert!(tool_result.success);
        assert_eq!(tool_result.content, "hello");
    }

    #[tokio::test]
    async fn test_not_found() {
        let registry = ToolRegistry::new();
        let result = registry
            .execute("nonexistent", &test_ctx(), &json!({}))
            .await;
        assert!(matches!(result, Err(ToolError::NotFound { .. })));
    }

    #[test]
    fn test_list() {
        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        struct NoopTool;
        #[async_trait]
        impl Tool for NoopTool {
            fn name(&self) -> &str {
                "noop"
            }
            fn description(&self) -> &str {
                "Does nothing"
            }
            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: &serde_json::Value,
            ) -> Result<ToolResult> {
                Ok(ToolResult::ok("ok"))
            }
        }
        registry.register(NoopTool);
        let tools = registry.list();
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|t| t.name() == "echo"));
        assert!(tools.iter().any(|t| t.name() == "noop"));
    }

    #[test]
    fn test_names() {
        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["echo"]);
    }

    #[test]
    fn test_remove() {
        let registry = ToolRegistry::new();
        registry.register(EchoTool);
        assert!(registry.get("echo").is_some());
        let removed = registry.remove("echo");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name(), "echo");
        assert!(registry.get("echo").is_none());
    }

    #[tokio::test]
    async fn test_execute_propagates_original_error() {
        struct FailingTool;
        #[async_trait]
        impl Tool for FailingTool {
            fn name(&self) -> &str {
                "failing"
            }
            fn description(&self) -> &str {
                "Always fails"
            }
            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: &serde_json::Value,
            ) -> Result<ToolResult> {
                Err(ToolError::InvalidParams {
                    tool: "failing".into(),
                    message: "bad input".into(),
                })
            }
        }
        let registry = ToolRegistry::new();
        registry.register(FailingTool);
        let result = registry.execute("failing", &test_ctx(), &json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidParams { .. })));
    }
}
