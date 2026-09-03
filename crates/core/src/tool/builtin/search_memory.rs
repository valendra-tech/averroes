use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::WorkDatabase;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct SearchMemoryTool {
    database: Arc<WorkDatabase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchMemoryParams {
    query: String,
    limit: Option<usize>,
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
        let params: SearchMemoryParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let query = params.query.trim();
        if query.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "query is required".into(),
            });
        }
        let limit = params.limit.unwrap_or(8);
        if !(1..=20).contains(&limit) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "limit must be between 1 and 20".into(),
            });
        }
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
