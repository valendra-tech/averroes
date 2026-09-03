use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct FileWriteTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteParams {
    file_path: String,
    content: String,
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file relative to the conversation's current directory or at an absolute path, including outside the workspace. Creates the file if it doesn't exist and overwrites it if it does."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path, or a path relative to the current directory. Parent paths such as ../other-project/file.txt are supported."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: FileWriteParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let file_path = params.file_path.as_str();
        let content = params.content.as_str();

        let full_path = resolve_file_path(&ctx.current_dir(), file_path);

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

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use super::*;
    use crate::tool::ToolActivation;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            working_dir: root.to_path_buf(),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[tokio::test]
    async fn writes_through_a_parent_path_outside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        FileWriteTool
            .execute(
                &context(&workspace),
                &json!({ "file_path": "../shared/output.txt", "content": "outside" }),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(directory.path().join("shared/output.txt")).unwrap(),
            "outside"
        );
    }
}
