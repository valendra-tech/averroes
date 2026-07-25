use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "The maximum number of lines to read"
                }
            },
            "required": ["file_path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let file_path = params["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: file_path".into(),
            })?;

        let full_path = ctx.working_dir.join(file_path);

        let content = tokio::fs::read_to_string(&full_path).await.map_err(|e| {
            ToolError::Execution {
                tool: self.name().into(),
                message: format!("Failed to read file '{}': {e}", full_path.display()),
            }
        })?;

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        let offset = params
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(1);

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let start = offset.max(1);
        let end = match limit {
            Some(lim) => (start.saturating_add(lim).min(total_lines + 1)).saturating_sub(1),
            None => total_lines,
        };

        let start_idx = (start.saturating_sub(1)).min(total_lines);
        let end_idx = end.min(total_lines);

        if start_idx >= total_lines {
            let output = format!(
                "File '{}' ({} lines total): requested offset {} exceeds file length",
                full_path.display(),
                total_lines,
                offset
            );
            return Ok(ToolResult::ok(output));
        }

        let selected = &all_lines[start_idx..end_idx];
        let line_range_header = if selected.len() == total_lines {
            format!("File '{}' ({} lines total):\n", full_path.display(), total_lines)
        } else {
            format!(
                "File '{}' (lines {}-{} of {}):\n",
                full_path.display(),
                start_idx + 1,
                end_idx,
                total_lines
            )
        };

        let mut body = String::new();
        for (i, line) in selected.iter().enumerate() {
            let line_num = start_idx + i + 1;
            body.push_str(&format!("{line_num}: {line}\n"));
        }

        Ok(ToolResult::ok(format!("{line_range_header}{body}")).with_metadata(json!({
            "total_lines": total_lines,
            "start": start_idx + 1,
            "end": end_idx,
            "file_path": full_path.display().to_string()
        })))
    }
}
