use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use std::path::Path;

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct FileReadTool;

const DEFAULT_LINE_LIMIT: usize = 400;
const MAX_LINE_LIMIT: usize = 2_000;
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read a text file by line range or return a supported image to a multimodal model. Relative paths use the conversation's current directory; absolute paths and paths outside the workspace are supported."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path, or a path relative to the current directory. Parent paths such as ../other-project/file.txt are supported."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 1,
                    "description": "The line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LINE_LIMIT,
                    "default": DEFAULT_LINE_LIMIT,
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

        let full_path = resolve_file_path(&ctx.current_dir(), file_path);

        if let Some(media_type) = image_media_type(&full_path) {
            return read_image(self.name(), &full_path, media_type).await;
        }

        let content =
            tokio::fs::read_to_string(&full_path)
                .await
                .map_err(|e| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!("Failed to read file '{}': {e}", full_path.display()),
                })?;

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        let offset = positive_integer_param(self.name(), params, "offset", 1, None)?;
        let limit = positive_integer_param(
            self.name(),
            params,
            "limit",
            DEFAULT_LINE_LIMIT,
            Some(MAX_LINE_LIMIT),
        )?;

        let start = offset;
        let end = (start.saturating_add(limit).min(total_lines + 1)).saturating_sub(1);

        let start_idx = (start.saturating_sub(1)).min(total_lines);
        let end_idx = end.min(total_lines);

        if start_idx >= total_lines {
            let output = format!(
                "File '{}' ({} lines total): requested offset {} exceeds file length",
                full_path.display(),
                total_lines,
                offset
            );
            return Ok(ToolResult::ok(output).with_metadata(json!({
                "total_lines": total_lines,
                "start": null,
                "end": null,
                "has_more": false,
                "next_offset": null,
                "file_path": full_path.display().to_string()
            })));
        }

        let selected = &all_lines[start_idx..end_idx];
        let line_range_header = if selected.len() == total_lines {
            format!(
                "File '{}' ({} lines total):\n",
                full_path.display(),
                total_lines
            )
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

        let has_more = end_idx < total_lines;
        let next_offset = has_more.then_some(end_idx + 1);
        if has_more {
            body.push_str(&format!(
                "\nMore lines available. Continue with offset {}.\n",
                next_offset.unwrap_or(end_idx + 1)
            ));
        }

        Ok(
            ToolResult::ok(format!("{line_range_header}{body}")).with_metadata(json!({
                "total_lines": total_lines,
                "start": start_idx + 1,
                "end": end_idx,
                "has_more": has_more,
                "next_offset": next_offset,
                "file_path": full_path.display().to_string()
            })),
        )
    }
}

async fn read_image(tool: &str, path: &Path, media_type: &str) -> Result<ToolResult> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| ToolError::Execution {
            tool: tool.into(),
            message: format!("Failed to read image '{}': {error}", path.display()),
        })?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ToolError::Execution {
            tool: tool.into(),
            message: format!(
                "Image '{}' exceeds the {} MiB tool-result limit",
                path.display(),
                MAX_IMAGE_BYTES / 1024 / 1024
            ),
        });
    }
    if !has_image_signature(media_type, &bytes) {
        return Err(ToolError::Execution {
            tool: tool.into(),
            message: format!(
                "File '{}' does not contain a valid {media_type} signature",
                path.display()
            ),
        });
    }

    Ok(ToolResult::ok(format!(
        "Image '{}' ({media_type}, {} bytes)",
        path.display(),
        bytes.len()
    ))
    .with_image(
        media_type,
        base64::engine::general_purpose::STANDARD.encode(bytes),
    )
    .with_metadata(json!({
        "file_path": path.display().to_string(),
        "media_type": media_type,
        "kind": "image"
    })))
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn has_image_signature(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => {
            bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|tag| tag == b"WEBP")
        }
        _ => false,
    }
}

fn positive_integer_param(
    tool: &str,
    params: &Value,
    name: &str,
    default: usize,
    maximum: Option<usize>,
) -> Result<usize> {
    let Some(value) = params.get(name) else {
        return Ok(default);
    };
    let value = value.as_u64().ok_or_else(|| ToolError::InvalidParams {
        tool: tool.into(),
        message: format!("{name} must be a positive integer"),
    })?;
    let value = usize::try_from(value).map_err(|_| ToolError::InvalidParams {
        tool: tool.into(),
        message: format!("{name} is too large"),
    })?;
    if value == 0 {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: format!("{name} must be at least 1"),
        });
    }
    if let Some(maximum) = maximum {
        if value > maximum {
            return Err(ToolError::InvalidParams {
                tool: tool.into(),
                message: format!("{name} must not exceed {maximum}"),
            });
        }
    }
    Ok(value)
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
    async fn defaults_to_a_bounded_page_and_returns_next_offset() {
        let directory = tempfile::tempdir().unwrap();
        let content = (1..=401)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(directory.path().join("large.txt"), content).unwrap();

        let result = FileReadTool
            .execute(
                &context(directory.path()),
                &json!({ "file_path": "large.txt" }),
            )
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert!(result.content.contains("400: line 400"));
        assert!(!result.content.contains("401: line 401"));
        assert_eq!(metadata["end"], 400);
        assert_eq!(metadata["has_more"], true);
        assert_eq!(metadata["next_offset"], 401);
    }

    #[tokio::test]
    async fn reads_a_requested_page() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("small.txt"),
            "one\ntwo\nthree\nfour\nfive\n",
        )
        .unwrap();

        let result = FileReadTool
            .execute(
                &context(directory.path()),
                &json!({ "file_path": "small.txt", "offset": 3, "limit": 2 }),
            )
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert!(result.content.contains("3: three"));
        assert!(result.content.contains("4: four"));
        assert!(!result.content.contains("5: five"));
        assert_eq!(metadata["start"], 3);
        assert_eq!(metadata["end"], 4);
        assert_eq!(metadata["next_offset"], 5);
    }

    #[tokio::test]
    async fn rejects_invalid_ranges() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("small.txt"), "one\n").unwrap();
        let ctx = context(directory.path());

        let offset_error = FileReadTool
            .execute(&ctx, &json!({ "file_path": "small.txt", "offset": 0 }))
            .await
            .unwrap_err();
        let limit_error = FileReadTool
            .execute(
                &ctx,
                &json!({ "file_path": "small.txt", "limit": MAX_LINE_LIMIT + 1 }),
            )
            .await
            .unwrap_err();

        assert!(matches!(offset_error, ToolError::InvalidParams { .. }));
        assert!(matches!(limit_error, ToolError::InvalidParams { .. }));
    }

    #[tokio::test]
    async fn reads_an_absolute_path_outside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let external = directory.path().join("shared.txt");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&external, "outside\n").unwrap();

        let result = FileReadTool
            .execute(
                &context(&workspace),
                &json!({ "file_path": external.to_string_lossy() }),
            )
            .await
            .unwrap();

        assert!(result.content.contains("1: outside"));
        assert_eq!(
            result.metadata.unwrap()["file_path"],
            external.display().to_string()
        );
    }

    #[tokio::test]
    async fn returns_supported_images_as_multimodal_content() {
        let directory = tempfile::tempdir().unwrap();
        let png = b"\x89PNG\r\n\x1a\nimage payload";
        std::fs::write(directory.path().join("chart.png"), png).unwrap();

        let result = FileReadTool
            .execute(
                &context(directory.path()),
                &json!({ "file_path": "chart.png" }),
            )
            .await
            .unwrap();

        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&result.images[0].data)
                .unwrap(),
            png
        );
        assert_eq!(result.metadata.unwrap()["kind"], "image");
    }
}
