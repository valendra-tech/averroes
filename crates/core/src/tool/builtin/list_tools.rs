use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct ListToolsTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListToolsParams {
    #[serde(default)]
    query: Option<String>,
}

#[async_trait]
impl Tool for ListToolsTool {
    fn name(&self) -> &str {
        "list_tools"
    }

    fn description(&self) -> &str {
        "Lists the tools currently available for this conversation and model. Use discover_tools when you need compact descriptions of the complete catalog."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional case-insensitive filter for a tool name or capability."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: ListToolsParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let query = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_ascii_lowercase);
        let mut tools = ctx
            .enabled_tools
            .iter()
            .filter(|tool| {
                query.as_ref().is_none_or(|query| {
                    tool.name.to_ascii_lowercase().contains(query)
                        || tool.description.to_ascii_lowercase().contains(query)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.name.cmp(&right.name));

        if tools.is_empty() {
            let content = if query.is_some() {
                "No enabled tools match that query."
            } else {
                "No tools are enabled for this conversation and model."
            };
            return Ok(ToolResult::ok(content).with_metadata(json!({ "tools": tools })));
        }
        let content = tools
            .iter()
            .map(|tool| format!("- **{}**: {}", tool.name, tool.description))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(content).with_metadata(json!({ "tools": tools })))
    }

    fn is_read_only(&self) -> bool {
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
            enabled_tools: vec![
                EnabledTool {
                    name: "file_read".into(),
                    description: "Reads a file".into(),
                },
                EnabledTool {
                    name: "web_search_intrernal".into(),
                    description: "Searches the web".into(),
                },
            ],
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[tokio::test]
    async fn lists_the_effective_tool_catalog_and_filters_it() {
        let tool = ListToolsTool;
        let all = tool.execute(&context(), &json!({})).await.unwrap();
        assert!(all.content.contains("file_read"));
        assert!(all.content.contains("web_search_intrernal"));

        let filtered = tool
            .execute(&context(), &json!({ "query": "web" }))
            .await
            .unwrap();
        assert!(!filtered.content.contains("file_read"));
        assert!(filtered.content.contains("web_search_intrernal"));
    }
}
