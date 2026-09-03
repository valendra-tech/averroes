use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GrepTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepParams {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    exclude: Option<String>,
    context: Option<usize>,
    limit: Option<usize>,
}

const DEFAULT_MATCH_LIMIT: usize = 100;
const MAX_MATCH_LIMIT: usize = 1_000;
const MAX_CONTEXT_LINES: usize = 10;
const MAX_OUTPUT_BYTES: usize = 64 * 1_024;
const MAX_CAPTURED_LINE_BYTES: usize = 16 * 1_024;

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
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: GrepParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let pattern = params.pattern.trim();
        if pattern.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "pattern is required".into(),
            });
        }
        let current_dir = ctx.current_dir();
        let root = params
            .path
            .as_deref()
            .map(|path| resolve_file_path(&current_dir, path))
            .unwrap_or(current_dir);
        let include = params.include;
        let exclude = params.exclude;
        let context = params.context.unwrap_or(0);
        if context > MAX_CONTEXT_LINES {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("context must be between 0 and {MAX_CONTEXT_LINES}"),
            });
        }
        let limit = params.limit.unwrap_or(DEFAULT_MATCH_LIMIT);
        if !(1..=MAX_MATCH_LIMIT).contains(&limit) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("limit must be between 1 and {MAX_MATCH_LIMIT}"),
            });
        }
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
    truncated_lines: usize,
    matches: usize,
    match_limit_reached: bool,
    output_limit_reached: bool,
}

struct SearchFileResult {
    binary: bool,
    output: String,
    matches: usize,
    match_limit_reached: bool,
    output_limit_reached: bool,
    truncated_lines: usize,
}

struct SearchWindow {
    lines: Vec<(usize, String, bool)>,
    end: usize,
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
    let (mut paths, walk_errors) = if root.is_file() {
        (
            path_allowed(display_root, root, include, exclude)
                .then(|| root.to_path_buf())
                .into_iter()
                .collect(),
            0,
        )
    } else {
        collect_paths(root, include, exclude)
    };
    paths.sort_by_key(|path| relative_path(display_root, path));

    let mut stats = SearchStats::default();
    stats.skipped_unreadable = walk_errors;
    let mut output = String::new();
    for path in paths {
        let remaining_limit = limit.saturating_sub(stats.matches);
        if remaining_limit == 0 {
            stats.match_limit_reached = true;
            break;
        }
        let file_display_path = relative_path(display_root, &path);
        let file_result =
            match search_file(&path, &file_display_path, regex, context, remaining_limit) {
                Ok(result) => result,
                Err(_) => {
                    stats.skipped_unreadable += 1;
                    continue;
                }
            };
        if file_result.binary {
            stats.skipped_binary += 1;
            continue;
        }
        stats.files_searched += 1;
        stats.truncated_lines += file_result.truncated_lines;
        stats.matches += file_result.matches;
        if file_result.matches > 0 {
            stats.files_with_matches += 1;
            if !push_bounded(&mut output, &file_result.output) {
                stats.output_limit_reached = true;
                break;
            }
        }
        if file_result.output_limit_reached {
            stats.output_limit_reached = true;
            break;
        }
        if file_result.match_limit_reached {
            stats.match_limit_reached = true;
            break;
        }
    }

    if output.is_empty() {
        output.push_str("No matches found");
    }
    add_completion_message(&mut output, &stats, limit);
    let truncated = stats.match_limit_reached || stats.output_limit_reached;
    Ok(ToolResult::ok(output).with_metadata(json!({
        "count": stats.matches,
        "matches": stats.matches,
        "files_searched": stats.files_searched,
        "files_with_matches": stats.files_with_matches,
        "skipped_binary": stats.skipped_binary,
        "skipped_unreadable": stats.skipped_unreadable,
        "truncated_lines": stats.truncated_lines,
        "truncated": truncated,
        "match_limit_reached": stats.match_limit_reached,
        "output_limit_reached": stats.output_limit_reached,
        "limit": limit,
        "context": context
    })))
}

fn search_file(
    path: &Path,
    display_path: &str,
    regex: &Regex,
    context: usize,
    remaining_limit: usize,
) -> io::Result<SearchFileResult> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut recent = VecDeque::with_capacity(context);
    let mut window: Option<SearchWindow> = None;
    let mut output = String::new();
    let mut matches = 0usize;
    let mut line_number = 0usize;
    let mut truncated_lines = 0usize;
    let mut match_limit_reached = false;
    let mut output_limit_reached = false;

    while let Some(line) = read_line_limited(&mut reader, MAX_CAPTURED_LINE_BYTES)? {
        if line.contains_nul {
            return Ok(SearchFileResult {
                binary: true,
                output: String::new(),
                matches: 0,
                match_limit_reached: false,
                output_limit_reached: false,
                truncated_lines: 0,
            });
        }
        let mut text = match String::from_utf8(line.bytes) {
            Ok(text) => text,
            Err(_) => {
                return Ok(SearchFileResult {
                    binary: true,
                    output: String::new(),
                    matches: 0,
                    match_limit_reached: false,
                    output_limit_reached: false,
                    truncated_lines: 0,
                })
            }
        };
        if text.ends_with('\r') {
            text.pop();
        }
        if line.truncated {
            truncated_lines = truncated_lines.saturating_add(1);
            text.push_str("... [line truncated]");
        }

        let is_match = regex.is_match(&text);
        if is_match && matches == remaining_limit {
            match_limit_reached = true;
            break;
        }

        let recent_text = text.clone();
        if let Some(current) = window.as_mut() {
            if line_number <= current.end {
                current.lines.push((line_number, text, is_match));
                if is_match {
                    matches = matches.saturating_add(1);
                    current.end = line_number.saturating_add(context);
                }
            } else {
                let current = window.take().expect("search window exists");
                if !append_search_window(&mut output, &display_path, &current) {
                    output_limit_reached = true;
                    break;
                }
                if is_match {
                    matches = matches.saturating_add(1);
                    window = Some(new_search_window(
                        line_number,
                        text,
                        is_match,
                        &recent,
                        context,
                    ));
                }
            }
        } else if is_match {
            matches = matches.saturating_add(1);
            window = Some(new_search_window(
                line_number,
                text,
                is_match,
                &recent,
                context,
            ));
        }

        if context > 0 {
            recent.push_back((line_number, recent_text));
            while recent.len() > context {
                recent.pop_front();
            }
        }
        line_number = line_number.saturating_add(1);
    }

    if !output_limit_reached {
        if let Some(current) = window {
            if !append_search_window(&mut output, &display_path, &current) {
                output_limit_reached = true;
            }
        }
    }

    Ok(SearchFileResult {
        binary: false,
        output,
        matches,
        match_limit_reached,
        output_limit_reached,
        truncated_lines,
    })
}

fn new_search_window(
    line_number: usize,
    text: String,
    is_match: bool,
    recent: &VecDeque<(usize, String)>,
    context: usize,
) -> SearchWindow {
    let mut lines = recent
        .iter()
        .map(|(number, text)| (*number, text.clone(), false))
        .collect::<Vec<_>>();
    lines.push((line_number, text, is_match));
    SearchWindow {
        lines,
        end: line_number.saturating_add(context),
    }
}

struct LimitedLine {
    bytes: Vec<u8>,
    truncated: bool,
    contains_nul: bool,
}

fn read_line_limited<R>(reader: &mut R, capture_limit: usize) -> io::Result<Option<LimitedLine>>
where
    R: BufRead,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut contains_nul = false;
    let mut saw_bytes = false;

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(saw_bytes.then_some(LimitedLine {
                bytes,
                truncated,
                contains_nul,
            }));
        }
        saw_bytes = true;
        contains_nul |= buffer.contains(&0);
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(buffer.len());
        let consumed = newline.map_or(buffer.len(), |position| position + 1);
        let remaining = capture_limit.saturating_sub(bytes.len());
        let copied = remaining.min(content_len);
        bytes.extend_from_slice(&buffer[..copied]);
        if copied < content_len {
            truncated = true;
        }
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(LimitedLine {
                bytes,
                truncated,
                contains_nul,
            }));
        }
    }
}

fn append_search_window(output: &mut String, path: &str, window: &SearchWindow) -> bool {
    if !output.is_empty() && !push_bounded(output, "--\n") {
        return false;
    }
    for (line_number, line, matched) in &window.lines {
        let separator = if *matched { ':' } else { '-' };
        let formatted = format!(
            "{path}{separator}{}{} {}\n",
            line_number + 1,
            separator,
            line
        );
        if !push_bounded(output, &formatted) {
            return false;
        }
    }
    true
}

fn collect_paths(
    root: &Path,
    include: Option<&str>,
    exclude: Option<&str>,
) -> (Vec<PathBuf>, usize) {
    let mut errors = 0usize;
    let paths = WalkBuilder::new(root)
        .standard_filters(true)
        .follow_links(false)
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(error) => {
                errors = errors.saturating_add(1);
                tracing::debug!(root = %root.display(), error = %error, "grep path could not be read");
                None
            }
        })
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| path_allowed(root, path, include, exclude))
        .collect();
    (paths, errors)
}

fn push_bounded(output: &mut String, value: &str) -> bool {
    if output.len().saturating_add(value.len()) > MAX_OUTPUT_BYTES {
        return false;
    }
    output.push_str(value);
    true
}

fn add_completion_message(output: &mut String, stats: &SearchStats, limit: usize) {
    let mut notices = Vec::new();
    if stats.output_limit_reached {
        notices.push(
            "Output truncated at 64 KiB. Narrow the pattern or use include/exclude filters."
                .to_owned(),
        );
    } else if stats.match_limit_reached {
        notices.push(format!(
            "Search stopped after {limit} matches. Narrow the pattern or use include/exclude filters."
        ));
    }
    if stats.skipped_binary > 0 {
        notices.push(format!(
            "Skipped {} binary or non-UTF-8 file(s).",
            stats.skipped_binary
        ));
    }
    if stats.skipped_unreadable > 0 {
        notices.push(format!(
            "Could not inspect {} path(s); results may be incomplete.",
            stats.skipped_unreadable
        ));
    }
    if stats.truncated_lines > 0 {
        notices.push(format!(
            "{} long line(s) were truncated before matching; results may be incomplete.",
            stats.truncated_lines
        ));
    }
    if notices.is_empty() {
        return;
    }
    let notice = format!("\n{}\n", notices.join("\n"));
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

    #[tokio::test]
    async fn reports_long_lines_that_were_truncated_before_matching() {
        let directory = tempfile::tempdir().unwrap();
        let long_line = format!(
            "needle{}\n",
            "x".repeat(MAX_CAPTURED_LINE_BYTES.saturating_add(100))
        );
        std::fs::write(directory.path().join("large.txt"), long_line).unwrap();

        let result = GrepTool
            .execute(&context(directory.path()), &json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert_eq!(metadata["matches"], 1);
        assert_eq!(metadata["truncated_lines"], 1);
        assert!(result.content.contains("[line truncated]"));
        assert!(result.content.contains("long line(s) were truncated"));
    }

    #[tokio::test]
    async fn reports_output_truncation_without_exceeding_the_limit() {
        let directory = tempfile::tempdir().unwrap();
        let content = (0..1_000)
            .map(|index| format!("hit {index} {}\n", "x".repeat(120)))
            .collect::<String>();
        std::fs::write(directory.path().join("many.txt"), content).unwrap();

        let result = GrepTool
            .execute(
                &context(directory.path()),
                &json!({ "pattern": "hit", "limit": 1_000 }),
            )
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();

        assert_eq!(metadata["output_limit_reached"], true);
        assert!(result.content.len() <= MAX_OUTPUT_BYTES);
        assert!(result.content.contains("Output truncated at 64 KiB"));
    }

    #[test]
    fn counts_directory_walk_errors() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");

        let (_, errors) = collect_paths(&missing, None, None);

        assert!(errors > 0);
    }
}
