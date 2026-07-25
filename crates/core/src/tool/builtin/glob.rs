use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match files against"
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let pattern = params["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: pattern".into(),
            })?;

        let search_path = ctx.working_dir.join(pattern);

        let paths: Vec<String> = glob::glob(&search_path.to_string_lossy())
            .map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Invalid glob pattern: {e}"),
            })?
            .filter_map(|entry| {
                let path = entry.ok()?;
                path.strip_prefix(&ctx.working_dir)
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .collect();

        let count = paths.len();

        if paths.is_empty() {
            Ok(ToolResult::ok("No files found").with_metadata(json!({ "count": count })))
        } else {
            let content = paths.join("\n");
            Ok(ToolResult::ok(content).with_metadata(json!({ "count": count })))
        }
    }
}
