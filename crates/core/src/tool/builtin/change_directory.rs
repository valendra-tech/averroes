use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct ChangeDirectoryTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeDirectoryParams {
    path: String,
}

#[async_trait]
impl Tool for ChangeDirectoryTool {
    fn name(&self) -> &str {
        "change_directory"
    }

    fn description(&self) -> &str {
        "Change the current directory for this conversation. Subsequent relative paths used by bash, file_read, file_write, patch, grep, glob, delegated agents, and shell-backed tools resolve from this directory. Absolute paths and directories outside the workspace are supported."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Existing directory. Accepts an absolute path, ~, or a path relative to the current directory."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: ChangeDirectoryParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let requested = params.path.trim();
        if requested.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "path cannot be empty".into(),
            });
        }

        let previous = ctx.current_dir();
        let candidate =
            resolve_directory(&previous, requested).ok_or_else(|| ToolError::Execution {
                tool: self.name().into(),
                message: "Could not resolve the home directory for '~'".into(),
            })?;
        let directory =
            tokio::fs::canonicalize(&candidate)
                .await
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!(
                        "Could not change directory to '{}': {error}",
                        candidate.display()
                    ),
                })?;
        let metadata =
            tokio::fs::metadata(&directory)
                .await
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!("Could not inspect '{}': {error}", directory.display()),
                })?;
        if !metadata.is_dir() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("'{}' is not a directory", directory.display()),
            });
        }

        ctx.set_current_dir(directory.clone());
        Ok(ToolResult::ok(format!(
            "Current directory changed to '{}'. Relative tool paths now resolve from here.",
            directory.display()
        ))
        .with_metadata(json!({
            "previous_directory": previous.display().to_string(),
            "current_directory": directory.display().to_string(),
            "workspace_root": ctx.working_dir.display().to_string()
        })))
    }
}

fn resolve_directory(current: &Path, requested: &str) -> Option<PathBuf> {
    if requested == "~" {
        return dirs::home_dir();
    }
    if let Some(relative) = requested.strip_prefix("~/") {
        return dirs::home_dir().map(|home| home.join(relative));
    }

    let requested = Path::new(requested);
    Some(if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        current.join(requested)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolActivation;
    use std::sync::Arc;

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
    async fn changes_the_base_for_relative_file_reads() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let nested = workspace.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("message.txt"), "from nested").unwrap();
        let context = context(&workspace);

        let result = ChangeDirectoryTool
            .execute(&context, &json!({ "path": "nested" }))
            .await
            .unwrap();
        let read = super::super::file_read::FileReadTool
            .execute(&context, &json!({ "file_path": "message.txt" }))
            .await
            .unwrap();

        assert_eq!(context.current_dir(), nested.canonicalize().unwrap());
        assert_eq!(
            result.metadata.unwrap()["workspace_root"],
            workspace.display().to_string()
        );
        assert!(read.content.contains("from nested"));
    }

    #[tokio::test]
    async fn failed_changes_keep_the_previous_directory() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(directory.path());
        let previous = context.current_dir();

        let result = ChangeDirectoryTool
            .execute(&context, &json!({ "path": "missing" }))
            .await;

        assert!(result.is_err());
        assert_eq!(context.current_dir(), previous);
    }

    #[tokio::test]
    async fn new_bash_sessions_start_in_the_changed_directory() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let nested = workspace.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let context = context(&workspace);

        ChangeDirectoryTool
            .execute(&context, &json!({ "path": "nested" }))
            .await
            .unwrap();
        let result = super::super::bash::BashTool::default()
            .execute(&context, &json!({ "command": "pwd" }))
            .await
            .unwrap();

        assert_eq!(
            result.content.trim(),
            nested.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(
            result.metadata.unwrap()["working_dir"],
            nested.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn expands_home_and_relative_paths_without_shell_parsing() {
        let current = Path::new("/tmp/project");
        assert_eq!(
            resolve_directory(current, "../shared").unwrap(),
            PathBuf::from("/tmp/project/../shared")
        );
        assert_eq!(
            resolve_directory(current, "/opt/code").unwrap(),
            PathBuf::from("/opt/code")
        );
        assert_eq!(resolve_directory(current, "~"), dirs::home_dir());
    }
}
