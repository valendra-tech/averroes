use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::WorkDatabase;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct SearchMemoryTool {
    database: Arc<WorkDatabase>,
}

impl SearchMemoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl Tool for SearchMemoryTool {
    fn name(&self) -> &str {
        "search_memory"
    }

    fn description(&self) -> &str {
        "Search compiled local conversation memory for relevant previous fragments. Use this when earlier work or decisions may help."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A concise phrase describing the earlier context to retrieve"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of conversation fragments to return"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "query is required".into(),
            })?;
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(8)
            .clamp(1, 20) as usize;
        let results = if let Some(backend) = ctx.memory_search_backend.as_ref() {
            backend
                .search(query, limit)
                .await
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: error,
                })?
        } else {
            self.database
                .search_conversations_text(query, limit)
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: error.to_string(),
                })?
        };
        if results.is_empty() {
            return Ok(ToolResult::ok("No relevant conversation memory found."));
        }
        let content = results
            .iter()
            .map(|result| format!("- **{}**\n  {}", result.title, result.snippet))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(content).with_metadata(json!({
            "source": "averroes conversation index",
            "result_count": results.len()
        })))
    }
}
