use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct DiscoverToolsTool;

#[derive(Deserialize)]
struct DiscoverToolsParams {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for DiscoverToolsTool {
    fn name(&self) -> &str {
        "discover_tools"
    }

    fn description(&self) -> &str {
        "Returns the complete registered tool catalog as compact name-and-description pairs. Choose the tools you need, then use enable_tools before invoking them."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Deprecated compatibility field; discovery always returns the complete catalog."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Deprecated compatibility field; the complete catalog is always returned."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: DiscoverToolsParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        // Keep accepting the old arguments so an already-running agent does
        // not fail its call, but never filter the catalog: omission here was
        // the reason agents believed registered tools did not exist.
        let _ = (params.query, params.limit);
        let mut tools = ctx.available_tools.clone();
        tools.sort_by(|left, right| left.name.cmp(&right.name));

        if tools.is_empty() {
            return Ok(ToolResult::ok("No tools are registered in this workspace.")
                .with_metadata(json!({ "tools": tools })));
        }

        let content = tools
            .iter()
            .map(|tool| format!("{}: {}", tool.name, tool.description))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(content).with_metadata(json!({ "tools": tools })))
    }

    fn is_read_only(&self) -> bool {
        true
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
            available_tools: vec![
                EnabledTool {
                    name: "file_read".into(),
                    description: "Reads a file from the workspace".into(),
                },
                EnabledTool {
                    name: "web_search_intrernal".into(),
                    description: "Searches the internet".into(),
                },
            ],
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[tokio::test]
    async fn returns_the_complete_catalog_without_returning_schemas() {
        let result = DiscoverToolsTool
            .execute(&context(), &json!({ "query": "internet" }))
            .await
            .unwrap();

        assert!(result.content.contains("web_search_intrernal"));
        assert!(result.content.contains("file_read"));
        assert!(result
            .content
            .contains("file_read: Reads a file from the workspace"));
        assert!(!result.content.contains("input_schema"));
    }
}
