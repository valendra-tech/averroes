use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GlobTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobParams {
    pattern: String,
    limit: Option<usize>,
}

const DEFAULT_MATCH_LIMIT: usize = 1_000;
const MAX_MATCH_LIMIT: usize = 10_000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern relative to the conversation's current directory or at an absolute path. Results are sorted and bounded."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Absolute glob pattern, or one relative to the current directory. Parent paths such as ../other-project/**/*.rs are supported."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_MATCH_LIMIT,
                    "default": DEFAULT_MATCH_LIMIT,
                    "description": "Maximum number of file paths to return"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: GlobParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let pattern = params.pattern.as_str();
        if pattern.trim().is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "pattern cannot be empty".into(),
            });
        }
        let limit = params.limit.unwrap_or(DEFAULT_MATCH_LIMIT);
        if !(1..=MAX_MATCH_LIMIT).contains(&limit) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("limit must be between 1 and {MAX_MATCH_LIMIT}"),
            });
        }

        let current_dir = ctx.current_dir();
        let search_path = resolve_file_path(&current_dir, pattern);

        let mut paths = BTreeSet::new();
        let mut matched_count = 0usize;
        let mut errors = 0usize;
        for entry in
            glob::glob(&search_path.to_string_lossy()).map_err(|e| ToolError::Execution {
                tool: self.name().into(),
                message: format!("Invalid glob pattern: {e}"),
            })?
        {
            let path = match entry {
                Ok(path) => path,
                Err(error) => {
                    errors = errors.saturating_add(1);
                    tracing::debug!(pattern = %pattern, error = %error, "glob entry could not be read");
                    continue;
                }
            };
            if !path.is_file() {
                continue;
            }
            matched_count = matched_count.saturating_add(1);
            let path = path
                .strip_prefix(&current_dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            paths.insert(path);
            if paths.len() > limit {
                paths.pop_last();
            }
        }

        let selected_count = paths.len();
        let mut content = String::new();
        let mut returned_count = 0usize;
        let mut output_truncated = false;
        for path in paths {
            if content.len().saturating_add(path.len()).saturating_add(1) > MAX_OUTPUT_BYTES {
                output_truncated = true;
                break;
            }
            content.push_str(&path);
            content.push('\n');
            returned_count = returned_count.saturating_add(1);
        }
        let truncated = matched_count > selected_count || output_truncated;
        if content.is_empty() {
            content.push_str("No files found");
        } else {
            content.pop();
        }
        if matched_count > selected_count && content.len() < MAX_OUTPUT_BYTES {
            content.push_str(&format!(
                "\n\nResults truncated to {limit} files. Narrow the pattern or increase limit."
            ));
        }
        if output_truncated && content.len() < MAX_OUTPUT_BYTES {
            content.push_str(
                "\n\nOutput truncated at 64 KiB. Narrow the pattern or use a narrower path.",
            );
        }
        if errors > 0 && content.len() < MAX_OUTPUT_BYTES {
            content.push_str(&format!(
                "\n\nCould not inspect {errors} matching path(s); results may be incomplete."
            ));
        }

        Ok(ToolResult::ok(content).with_metadata(json!({
            "count": returned_count,
            "matched_count": matched_count,
            "limit": limit,
            "truncated": truncated,
            "output_truncated": output_truncated,
            "errors": errors,
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

    #[tokio::test]
    async fn reports_paths_omitted_by_the_output_byte_limit() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..500 {
            let name = format!("file_{index:04}_{}.txt", "x".repeat(160));
            std::fs::write(directory.path().join(name), "match").unwrap();
        }

        let result = GlobTool
            .execute(
                &context(directory.path()),
                &json!({ "pattern": "*.txt", "limit": 1_000 }),
            )
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert_eq!(metadata["matched_count"], 500);
        assert_eq!(metadata["output_truncated"], true);
        assert_eq!(metadata["truncated"], true);
        assert!(metadata["count"].as_u64().unwrap() < 500);
        assert!(result.content.contains("Output truncated at 64 KiB"));
    }
}
