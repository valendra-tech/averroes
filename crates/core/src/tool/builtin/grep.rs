use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents using a regular expression pattern"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regular expression pattern to search for in file contents"
                },
                "include": {
                    "type": "string",
                    "description": "Optional file name glob pattern to filter which files to search"
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let pattern_str = params["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: pattern".into(),
            })?;

        let include_pattern = params.get("include").and_then(|v| v.as_str());

        let re = regex::Regex::new(pattern_str).map_err(|e| ToolError::InvalidParams {
            tool: self.name().into(),
            message: format!("Invalid regex pattern: {e}"),
        })?;

        let mut results: Vec<String> = Vec::new();

        let mut entries =
            tokio::fs::read_dir(&ctx.working_dir)
                .await
                .map_err(|e| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!("Failed to read working directory: {e}"),
                })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Failed to read directory entry: {e}"),
            })?
        {
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            if path.is_symlink() {
                continue;
            }

            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(inc) = include_pattern {
                    if !glob_match::glob_match(inc, file_name) {
                        continue;
                    }
                }

                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => {
                        let rel_path = path
                            .strip_prefix(&ctx.working_dir)
                            .unwrap_or(&path)
                            .display()
                            .to_string();

                        for (line_num, line_content) in content.lines().enumerate() {
                            if re.is_match(line_content) {
                                results.push(format!(
                                    "{}:{}: {}",
                                    rel_path,
                                    line_num + 1,
                                    line_content
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read file '{}': {e}", path.display());
                        continue;
                    }
                }
            }
        }

        let count = results.len();

        if results.is_empty() {
            Ok(ToolResult::ok("No matches found").with_metadata(json!({ "count": count })))
        } else {
            let content = results.join("\n");
            Ok(ToolResult::ok(content).with_metadata(json!({ "count": count })))
        }
    }
}
