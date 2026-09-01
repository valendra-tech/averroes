use crate::agent::orchestration::AgentDescriptor;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;

pub struct ListAgentsTool;

#[async_trait]
impl Tool for ListAgentsTool {
    fn name(&self) -> &str {
        "list_agents"
    }

    fn description(&self) -> &str {
        "Lists the delegated agents available for this conversation. The default agent is always available."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, _params: &serde_json::Value) -> Result<ToolResult> {
        let agents = match ctx.agent_runner.as_ref() {
            Some(runner) => runner
                .list_agents(&ctx.session_id)
                .await
                .map_err(|message| ToolError::Execution {
                    tool: self.name().into(),
                    message,
                })?,
            None => vec![AgentDescriptor::default()],
        };
        let content =
            serde_json::to_string_pretty(&agents).map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        Ok(ToolResult::ok(content).with_metadata(json!({ "agents": agents })))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}
