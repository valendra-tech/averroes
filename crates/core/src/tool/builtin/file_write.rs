use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let file_path = params["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: file_path".into(),
            })?;

        let content = params["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: content".into(),
            })?;

        let full_path = ctx.working_dir.join(file_path);

        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!(
                        "Failed to create parent directories for '{}': {e}",
                        full_path.display()
                    ),
                })?;
        }

        tokio::fs::write(&full_path, content)
            .await
            .map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Failed to write file '{}': {e}", full_path.display()),
            })?;

        let byte_count = content.len();
        Ok(ToolResult::ok(format!(
            "Successfully wrote {} bytes to '{}'",
            byte_count,
            full_path.display()
        ))
        .with_metadata(json!({
            "byte_count": byte_count,
            "file_path": full_path.display().to_string()
        })))
    }
}
