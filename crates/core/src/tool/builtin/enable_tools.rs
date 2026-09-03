use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct EnableToolsTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnableToolsParams {
    names: Vec<String>,
}

#[async_trait]
impl Tool for EnableToolsTool {
    fn name(&self) -> &str {
        "enable_tools"
    }

    fn description(&self) -> &str {
        "Compatibility tool. All registered tools are already available from the first turn, so no activation is required."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Tool names to validate for compatibility."
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
        let requested = params
            .names
            .into_iter()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        let missing = requested
            .iter()
            .filter(|name| {
                !ctx.available_tools
                    .iter()
                    .any(|tool| tool.name.as_str() == name.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("unknown tool name(s): {}", missing.join(", ")),
            });
        }
        let names = ctx
            .available_tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        Ok(ToolResult::ok(format!(
            "All {} registered tool(s) are already available; no activation is needed.",
            names.len()
        ))
        .with_metadata(json!({ "enabled_tools": names })))
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
    async fn reports_that_registered_tools_are_already_available() {
        let context = context();
        let result = EnableToolsTool
            .execute(&context, &json!({ "names": ["web_search_intrernal"] }))
            .await
            .unwrap();

        assert!(result.content.contains("already available"));
        assert!(context.tool_activation.names().is_empty());
    }
}
