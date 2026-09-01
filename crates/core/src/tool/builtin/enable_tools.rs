use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct EnableToolsTool;

#[derive(Deserialize)]
struct EnableToolsParams {
    names: Vec<String>,
}

#[async_trait]
impl Tool for EnableToolsTool {
    fn name(&self) -> &str {
        "enable_tools"
    }

    fn description(&self) -> &str {
        "Activates one or more tools returned by discover_tools. Their full schemas become available on the next agent step and remain enabled for this conversation."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Exact tool names returned by discover_tools."
                }
            },
            "required": ["names"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: EnableToolsParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        if params.names.iter().all(|name| name.trim().is_empty()) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "names must contain at least one tool name".into(),
            });
        }
        let enabled_tools = ctx
            .tool_activation
            .enable(&ctx.available_tools, params.names)?;
        let names = enabled_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        Ok(ToolResult::ok(format!(
            "Enabled {} tool(s): {}. Their schemas are available on the next step.",
            names.len(),
            names.join(", ")
        ))
        .with_metadata(json!({ "enabled_tools": names })))
    }

    fn is_bootstrap(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{EnabledTool, ToolActivation};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn context() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: vec![EnabledTool {
                name: "web_search_intrernal".into(),
                description: "Searches the internet".into(),
            }],
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[tokio::test]
    async fn enables_only_registered_tools() {
        let context = context();
        let result = EnableToolsTool
            .execute(&context, &json!({ "names": ["web_search_intrernal"] }))
            .await
            .unwrap();

        assert!(result.content.contains("web_search_intrernal"));
        assert_eq!(
            context.tool_activation.names(),
            vec!["web_search_intrernal"]
        );
    }
}
