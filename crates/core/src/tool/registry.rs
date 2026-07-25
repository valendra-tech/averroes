use super::{Result, Tool, ToolContext, ToolError, ToolRef, ToolResult};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;

pub struct ToolRegistry {
    tools: DashMap<String, ToolRef>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: DashMap::new() }
    }

    pub fn register(&self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<ToolRef> {
        self.tools.get(name).map(|r| r.clone())
    }

    pub fn list(&self) -> Vec<ToolRef> {
        self.tools.iter().map(|r| r.clone()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|r| r.key().clone()).collect()
    }

    pub fn remove(&self, name: &str) -> Option<ToolRef> {
        self.tools.remove(name).map(|(_, v)| v)
    }

    pub async fn execute(&self, name: &str, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let tool = self.get(name).ok_or_else(|| ToolError::NotFound { tool: name.to_string() })?;
        tool.execute(ctx, params).await.map_err(|e| ToolError::Execution {
            tool: name.to_string(),
            message: e.to_string(),
        })
    }
}

impl Default for ToolRegistry {
    fn default() -> Self { Self::new() }
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
        fn name(&self) -> &str { "echo" }

        fn description(&self) -> &str { "Echoes back the input" }

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

        async fn execute(&self, _ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
            let text = params["text"].as_str().unwrap_or("");
            Ok(ToolResult::ok(text))
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "test-session".into(),
            agent_id: "test-agent".into(),
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
        let result = registry.execute("echo", &test_ctx(), &json!({"text": "hello"})).await;
        assert!(result.is_ok());
        let tool_result = result.unwrap();
        assert!(tool_result.success);
        assert_eq!(tool_result.content, "hello");
    }

    #[tokio::test]
    async fn test_not_found() {
        let registry = ToolRegistry::new();
        let result = registry.execute("nonexistent", &test_ctx(), &json!({})).await;
        assert!(result.is_err());
    }
}
