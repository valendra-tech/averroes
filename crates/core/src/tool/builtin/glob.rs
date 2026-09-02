use async_trait::async_trait;
use serde_json::{json, Value};

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern relative to the conversation's current directory or at an absolute path"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Absolute glob pattern, or one relative to the current directory. Parent paths such as ../other-project/**/*.rs are supported."
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

        let current_dir = ctx.current_dir();
        let search_path = resolve_file_path(&current_dir, pattern);

        let paths: Vec<String> = glob::glob(&search_path.to_string_lossy())
            .map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Invalid glob pattern: {e}"),
            })?
            .filter_map(|entry| {
                let path = entry.ok()?;
                Some(
                    path.strip_prefix(&current_dir)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                )
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
    async fn returns_matches_for_an_absolute_pattern_outside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let external = directory.path().join("shared/match.txt");
        std::fs::create_dir_all(external.parent().unwrap()).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&external, "match").unwrap();
        let pattern = directory.path().join("shared/*.txt");

        let result = GlobTool
            .execute(
                &context(&workspace),
                &json!({ "pattern": pattern.to_string_lossy() }),
            )
            .await
            .unwrap();

        assert_eq!(result.content, external.display().to_string());
        assert_eq!(result.metadata.unwrap()["count"], 1);
    }
}
