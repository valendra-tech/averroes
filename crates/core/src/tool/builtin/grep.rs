use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GrepTool;

const DEFAULT_MATCH_LIMIT: usize = 100;
const MAX_MATCH_LIMIT: usize = 1_000;
const MAX_CONTEXT_LINES: usize = 10;
const MAX_OUTPUT_BYTES: usize = 64 * 1_024;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Recursively search text files using a regular expression from the conversation's current directory or an absolute path"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search. Accepts an absolute path or a path relative to the current directory, including ../. Defaults to the current directory."
                },
                "include": {
                    "type": "string",
                    "description": "Optional glob for paths or file names to include, such as '**/*.rs'"
                },
                "exclude": {
                    "type": "string",
                    "description": "Optional glob for paths or file names to exclude"
                },
                "context": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_CONTEXT_LINES,
                    "default": 0,
                    "description": "Surrounding lines before and after each match"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_MATCH_LIMIT,
                    "default": DEFAULT_MATCH_LIMIT,
                    "description": "Maximum number of matching lines"
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let pattern = required_string(self.name(), params, "pattern")?;
        let current_dir = ctx.current_dir();
        let root = optional_string(self.name(), params, "path")?
            .map(|path| resolve_file_path(&current_dir, path))
            .unwrap_or(current_dir);
        let include = optional_string(self.name(), params, "include")?.map(str::to_owned);
        let exclude = optional_string(self.name(), params, "exclude")?.map(str::to_owned);
        let context = integer_param(self.name(), params, "context", 0, 0, MAX_CONTEXT_LINES)?;
        let limit = integer_param(
            self.name(),
            params,
            "limit",
            DEFAULT_MATCH_LIMIT,
            1,
            MAX_MATCH_LIMIT,
        )?;
        let regex = Regex::new(pattern).map_err(|error| ToolError::InvalidParams {
            tool: self.name().into(),
            message: format!("Invalid regex pattern: {error}"),
        })?;
        tokio::task::spawn_blocking(move || {
            search_workspace(
                &root,
                &regex,
                include.as_deref(),
                exclude.as_deref(),
                context,
                limit,
            )
        })
        .await
        .map_err(|error| ToolError::Execution {
            tool: self.name().into(),
            message: format!("Search task failed: {error}"),
        })?
    }
}

#[derive(Default)]
struct SearchStats {
    files_searched: usize,
    files_with_matches: usize,
    skipped_binary: usize,
    skipped_unreadable: usize,
    matches: usize,
    match_limit_reached: bool,
    output_limit_reached: bool,
}

struct FileMatches {
    path: String,
    lines: Vec<String>,
    matching_lines: Vec<usize>,
}

fn search_workspace(
    root: &Path,
    regex: &Regex,
    include: Option<&str>,
    exclude: Option<&str>,
    context: usize,
    limit: usize,
) -> Result<ToolResult> {
    if !root.is_dir() && !root.is_file() {
        return Err(ToolError::Execution {
            tool: "grep".into(),
            message: format!("Search path '{}' is not accessible", root.display()),
        });
    }

    let display_root = if root.is_file() {
        root.parent().unwrap_or_else(|| Path::new(""))
    } else {
        root
    };
    let mut paths = if root.is_file() {
        path_allowed(display_root, root, include, exclude)
            .then(|| root.to_path_buf())
            .into_iter()
            .collect()
    } else {
        collect_paths(root, include, exclude)
    };
    paths.sort_by_key(|path| relative_path(display_root, path));

    let mut stats = SearchStats::default();
    let mut output = String::new();
    for path in paths {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                stats.skipped_unreadable += 1;
                continue;
            }
        };
        if bytes.contains(&0) {
            stats.skipped_binary += 1;
            continue;
        }
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content,
            Err(_) => {
                stats.skipped_binary += 1;
                continue;
            }
        };
        stats.files_searched += 1;

        let lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
        let mut matching_lines = Vec::new();
        let mut stop_after_file = false;
        for (index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            if stats.matches == limit {
                stats.match_limit_reached = true;
                stop_after_file = true;
                break;
            }
            stats.matches += 1;
            matching_lines.push(index);
        }
        if !matching_lines.is_empty() {
            stats.files_with_matches += 1;
            let file = FileMatches {
                path: relative_path(display_root, &path),
                lines,
                matching_lines,
            };
            if !append_file_matches(&mut output, &file, context) {
                stats.output_limit_reached = true;
                break;
            }
        }
        if stop_after_file {
            break;
        }
    }

    add_completion_message(&mut output, &stats, limit);

    if output.is_empty() {
        output.push_str("No matches found");
    }
    let truncated = stats.match_limit_reached || stats.output_limit_reached;
    Ok(ToolResult::ok(output).with_metadata(json!({
        "count": stats.matches,
        "matches": stats.matches,
        "files_searched": stats.files_searched,
        "files_with_matches": stats.files_with_matches,
        "skipped_binary": stats.skipped_binary,
        "skipped_unreadable": stats.skipped_unreadable,
        "truncated": truncated,
        "match_limit_reached": stats.match_limit_reached,
        "output_limit_reached": stats.output_limit_reached,
        "limit": limit,
        "context": context
    })))
}

fn collect_paths(root: &Path, include: Option<&str>, exclude: Option<&str>) -> Vec<PathBuf> {
    WalkBuilder::new(root)
        .standard_filters(true)
        .follow_links(false)
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| path_allowed(root, path, include, exclude))
        .collect()
}

fn append_file_matches(output: &mut String, file: &FileMatches, context: usize) -> bool {
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for &line in &file.matching_lines {
        let start = line.saturating_sub(context);
        let end = line.saturating_add(context + 1).min(file.lines.len());
        if let Some((_, previous_end)) = windows.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        windows.push((start, end));
    }

    for (window_index, (start, end)) in windows.into_iter().enumerate() {
        if window_index > 0 && !push_bounded(output, "--\n") {
            return false;
        }
        for line_index in start..end {
            let matched = file.matching_lines.binary_search(&line_index).is_ok();
            let separator = if matched { ':' } else { '-' };
            let line = format!(
                "{}{}{}{} {}\n",
                file.path,
                separator,
                line_index + 1,
                separator,
                file.lines[line_index]
            );
            if !push_bounded(output, &line) {
                return false;
            }
        }
    }
    true
}

fn push_bounded(output: &mut String, value: &str) -> bool {
    if output.len().saturating_add(value.len()) > MAX_OUTPUT_BYTES {
        return false;
    }
    output.push_str(value);
    true
}

fn add_completion_message(output: &mut String, stats: &SearchStats, limit: usize) {
    let notice = if stats.output_limit_reached {
        "\nOutput truncated at 64 KiB. Narrow the pattern or use include/exclude filters.\n"
            .to_owned()
    } else if stats.match_limit_reached {
        format!(
            "\nSearch stopped after {limit} matches. Narrow the pattern or use include/exclude filters.\n"
        )
    } else {
        return;
    };
    while output.len().saturating_add(notice.len()) > MAX_OUTPUT_BYTES {
        if output.pop().is_none() {
            break;
        }
    }
    output.push_str(&notice);
}

fn path_allowed(root: &Path, path: &Path, include: Option<&str>, exclude: Option<&str>) -> bool {
    let relative = relative_path(root, path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let matches = |pattern: &str| {
        glob_match::glob_match(pattern, &relative) || glob_match::glob_match(pattern, file_name)
    };
    include.is_none_or(matches) && !exclude.is_some_and(matches)
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn required_string<'a>(tool: &str, params: &'a Value, name: &str) -> Result<&'a str> {
    params
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidParams {
            tool: tool.into(),
            message: format!("Missing required parameter: {name}"),
        })
}

fn optional_string<'a>(tool: &str, params: &'a Value, name: &str) -> Result<Option<&'a str>> {
    match params.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidParams {
                tool: tool.into(),
                message: format!("{name} must be a string"),
            }),
    }
}

fn integer_param(
    tool: &str,
    params: &Value,
    name: &str,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize> {
    let Some(value) = params.get(name) else {
        return Ok(default);
    };
    let value = value.as_u64().ok_or_else(|| ToolError::InvalidParams {
        tool: tool.into(),
        message: format!("{name} must be an integer between {minimum} and {maximum}"),
    })?;
    let value = usize::try_from(value).map_err(|_| ToolError::InvalidParams {
        tool: tool.into(),
        message: format!("{name} is too large"),
    })?;
    if !(minimum..=maximum).contains(&value) {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: format!("{name} must be between {minimum} and {maximum}"),
        });
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
    async fn searches_recursively_with_filters_and_context() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("src")).unwrap();
        std::fs::write(
            directory.path().join("src/main.rs"),
            "before\nneedle\nafter\n",
        )
        .unwrap();
        std::fs::write(directory.path().join("src/generated.rs"), "needle\n").unwrap();
        std::fs::write(directory.path().join("notes.txt"), "needle\n").unwrap();

        let result = GrepTool
            .execute(
                &context(directory.path()),
                &json!({
                    "pattern": "needle",
                    "include": "**/*.rs",
                    "exclude": "**/generated.rs",
                    "context": 1
                }),
            )
            .await
            .unwrap();

        assert!(result.content.contains("src/main.rs-1- before"));
        assert!(result.content.contains("src/main.rs:2: needle"));
        assert!(result.content.contains("src/main.rs-3- after"));
        assert!(!result.content.contains("generated.rs"));
        assert!(!result.content.contains("notes.txt"));
        assert_eq!(result.metadata.unwrap()["matches"], 1);
    }

    #[tokio::test]
    async fn silently_skips_non_utf8_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("text.txt"), "needle\n").unwrap();
        std::fs::write(directory.path().join("image.png"), [0xff, 0xfe, 0xfd]).unwrap();

        let result = GrepTool
            .execute(&context(directory.path()), &json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert!(result.content.contains("text.txt:1: needle"));
        assert_eq!(metadata["matches"], 1);
        assert_eq!(metadata["skipped_binary"], 1);
    }

    #[tokio::test]
    async fn stops_at_the_match_limit() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("many.txt"), "hit\nhit\nhit\n").unwrap();

        let result = GrepTool
            .execute(
                &context(directory.path()),
                &json!({ "pattern": "hit", "limit": 2 }),
            )
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert_eq!(metadata["matches"], 2);
        assert_eq!(metadata["match_limit_reached"], true);
        assert_eq!(metadata["truncated"], true);
        assert!(result.content.contains("many.txt:1: hit"));
        assert!(result.content.contains("many.txt:2: hit"));
        assert!(!result.content.contains("many.txt:3: hit"));
    }

    #[tokio::test]
    async fn searches_a_directory_outside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let external = directory.path().join("shared");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&external).unwrap();
        std::fs::write(external.join("outside.txt"), "external needle\n").unwrap();

        let result = GrepTool
            .execute(
                &context(&workspace),
                &json!({ "pattern": "needle", "path": "../shared" }),
            )
            .await
            .unwrap();

        assert!(result.content.contains("outside.txt:1: external needle"));
        assert_eq!(result.metadata.unwrap()["matches"], 1);
    }

    #[tokio::test]
    async fn searches_an_absolute_file_outside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let external = directory.path().join("outside.txt");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&external, "one needle\n").unwrap();

        let result = GrepTool
            .execute(
                &context(&workspace),
                &json!({ "pattern": "needle", "path": external.to_string_lossy() }),
            )
            .await
            .unwrap();

        assert!(result.content.contains("outside.txt:1: one needle"));
    }
}
