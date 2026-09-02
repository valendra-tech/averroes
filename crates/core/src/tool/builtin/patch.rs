use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

/// Applies compact unified diffs, including the `*** Begin Patch` format used
/// by coding agents. The tool keeps the patch itself as the only large input
/// and returns a short result instead of echoing changed file contents.
pub struct PatchTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOperation {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Debug, Clone)]
struct FilePatch {
    operation: FileOperation,
    path: String,
    destination: Option<String>,
    hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    lines: Vec<PatchLine>,
}

#[derive(Debug, Clone)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

#[derive(Debug)]
struct PreparedChange {
    operation: FileOperation,
    source: PathBuf,
    destination: PathBuf,
    content: Option<String>,
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a compact unified diff relative to the conversation's current directory or to absolute paths, including outside the workspace. Accepts ../ paths, the *** Begin Patch format, or standard unified diff format. Use this instead of rewriting complete files."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Unified diff, preferably using *** Begin Patch with Update File, Add File, or Delete File sections."
                }
            },
            "required": ["patch"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let patch = params
            .get("patch")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_params("Missing required parameter: patch"))?;
        if patch.trim().is_empty() {
            return Err(invalid_params("patch cannot be empty"));
        }
        if patch.len() > 10 * 1024 * 1024 {
            return Err(invalid_params("patch cannot exceed 10 MB"));
        }

        let file_patches = parse_patch(patch).map_err(invalid_params)?;
        let current_dir = ctx.current_dir();
        let changes = prepare_changes(&current_dir, file_patches)
            .await
            .map_err(|message| ToolError::Execution {
                tool: self.name().into(),
                message,
            })?;

        for change in &changes {
            match change.operation {
                FileOperation::Delete => {
                    tokio::fs::remove_file(&change.source)
                        .await
                        .map_err(|error| execution_error(&change.source, error))?;
                }
                FileOperation::Add | FileOperation::Update | FileOperation::Move => {
                    if let Some(parent) = change.destination.parent() {
                        tokio::fs::create_dir_all(parent)
                            .await
                            .map_err(|error| execution_error(parent, error))?;
                    }
                    tokio::fs::write(
                        &change.destination,
                        change.content.as_deref().unwrap_or_default(),
                    )
                    .await
                    .map_err(|error| execution_error(&change.destination, error))?;

                    if change.operation == FileOperation::Move {
                        tokio::fs::remove_file(&change.source)
                            .await
                            .map_err(|error| execution_error(&change.source, error))?;
                    }
                }
            }
        }

        let paths = changes
            .iter()
            .map(|change| {
                change
                    .destination
                    .strip_prefix(&current_dir)
                    .unwrap_or(&change.destination)
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>();
        Ok(ToolResult::ok(format!(
            "Patch applied successfully to {} file(s).",
            changes.len()
        ))
        .with_metadata(json!({
            "files_changed": changes.len(),
            "paths": paths,
        })))
    }
}

fn invalid_params(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParams {
        tool: "patch".into(),
        message: message.into(),
    }
}

fn execution_error(path: &Path, error: std::io::Error) -> ToolError {
    ToolError::Execution {
        tool: "patch".into(),
        message: format!("Failed to update '{}': {error}", path.display()),
    }
}

async fn prepare_changes(
    workspace: &Path,
    patches: Vec<FilePatch>,
) -> std::result::Result<Vec<PreparedChange>, String> {
    let mut changes = Vec::with_capacity(patches.len());
    let mut seen = Vec::with_capacity(patches.len());

    for file_patch in patches {
        let source = resolve_patch_path(workspace, &file_patch.path)?;
        let destination = match file_patch.destination.as_deref() {
            Some(path) => resolve_patch_path(workspace, path)?,
            None => source.clone(),
        };
        if seen
            .iter()
            .any(|path: &PathBuf| path == &source || path == &destination)
        {
            return Err(format!(
                "patch contains overlapping changes for '{}'",
                source.display()
            ));
        }
        seen.push(source.clone());
        seen.push(destination.clone());

        let source_exists = tokio::fs::try_exists(&source)
            .await
            .map_err(|error| format!("cannot inspect '{}': {error}", source.display()))?;
        let destination_exists = tokio::fs::try_exists(&destination)
            .await
            .map_err(|error| format!("cannot inspect '{}': {error}", destination.display()))?;

        match file_patch.operation {
            FileOperation::Add => {
                if destination_exists {
                    return Err(format!(
                        "cannot add '{}': file already exists",
                        destination.display()
                    ));
                }
                let content = render_added_file(&file_patch.hunks)?;
                changes.push(PreparedChange {
                    operation: FileOperation::Add,
                    source: destination.clone(),
                    destination,
                    content: Some(content),
                });
            }
            FileOperation::Update | FileOperation::Move => {
                if !source_exists {
                    return Err(format!(
                        "file to update was not found: {}",
                        source.display()
                    ));
                }
                if file_patch.operation == FileOperation::Move && destination_exists {
                    return Err(format!(
                        "cannot move to '{}': file already exists",
                        destination.display()
                    ));
                }
                let original = tokio::fs::read_to_string(&source)
                    .await
                    .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
                let content = apply_hunks(&original, &file_patch.hunks)?;
                changes.push(PreparedChange {
                    operation: file_patch.operation,
                    source,
                    destination,
                    content: Some(content),
                });
            }
            FileOperation::Delete => {
                if !source_exists {
                    return Err(format!(
                        "file to delete was not found: {}",
                        source.display()
                    ));
                }
                if !file_patch.hunks.is_empty() {
                    let original = tokio::fs::read_to_string(&source)
                        .await
                        .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
                    let remaining = apply_hunks(&original, &file_patch.hunks)?;
                    if !remaining.is_empty() {
                        return Err(format!(
                            "delete patch did not remove all contents from '{}'",
                            source.display()
                        ));
                    }
                }
                changes.push(PreparedChange {
                    operation: FileOperation::Delete,
                    source: source.clone(),
                    destination: source,
                    content: None,
                });
            }
        }
    }

    Ok(changes)
}

fn resolve_patch_path(workspace: &Path, raw_path: &str) -> std::result::Result<PathBuf, String> {
    let path = raw_path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("patch contains an empty file path".into());
    }
    Ok(resolve_file_path(workspace, path))
}

fn parse_patch(input: &str) -> std::result::Result<Vec<FilePatch>, String> {
    let lines = input
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    if lines.iter().any(|line| line.starts_with("*** Begin Patch")) {
        parse_agent_patch(&lines)
    } else {
        parse_unified_diff(&lines)
    }
}

fn parse_agent_patch(lines: &[&str]) -> std::result::Result<Vec<FilePatch>, String> {
    let mut patches = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut hunk: Option<Hunk> = None;

    for line in lines {
        if line.starts_with("*** Begin Patch") {
            continue;
        }
        if line.starts_with("*** End Patch") {
            finish_hunk(&mut current, &mut hunk)?;
            finish_file(&mut patches, &mut current)?;
            break;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            finish_hunk(&mut current, &mut hunk)?;
            finish_file(&mut patches, &mut current)?;
            current = Some(FilePatch {
                operation: FileOperation::Add,
                path: path.trim().into(),
                destination: None,
                hunks: Vec::new(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            finish_hunk(&mut current, &mut hunk)?;
            finish_file(&mut patches, &mut current)?;
            current = Some(FilePatch {
                operation: FileOperation::Delete,
                path: path.trim().into(),
                destination: None,
                hunks: Vec::new(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            finish_hunk(&mut current, &mut hunk)?;
            finish_file(&mut patches, &mut current)?;
            current = Some(FilePatch {
                operation: FileOperation::Update,
                path: path.trim().into(),
                destination: None,
                hunks: Vec::new(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            let file = current
                .as_mut()
                .ok_or_else(|| "*** Move to: must follow a file section".to_string())?;
            if file.operation != FileOperation::Update {
                return Err("*** Move to: is only valid after *** Update File:".into());
            }
            file.operation = FileOperation::Move;
            file.destination = Some(path.trim().into());
            continue;
        }
        if line.starts_with("@@") {
            finish_hunk(&mut current, &mut hunk)?;
            if current.is_none() {
                return Err("patch hunk appears before a file section".into());
            }
            hunk = Some(parse_hunk_header(line)?);
            continue;
        }
        if *line == "\\ No newline at end of file" || line.starts_with("*** End of File") {
            continue;
        }
        if let Some(file) = current.as_ref() {
            if file.operation == FileOperation::Add && hunk.is_none() && line.starts_with('+') {
                hunk = Some(Hunk {
                    old_start: 0,
                    lines: Vec::new(),
                });
            }
        }
        if let Some(hunk) = hunk.as_mut() {
            hunk.lines.push(parse_patch_line(line)?);
        } else if !line.trim().is_empty() {
            return Err(format!("unexpected line in patch: {line}"));
        }
    }

    finish_hunk(&mut current, &mut hunk)?;
    finish_file(&mut patches, &mut current)?;
    if patches.is_empty() {
        return Err("patch did not contain any file sections".into());
    }
    Ok(patches)
}

fn parse_unified_diff(lines: &[&str]) -> std::result::Result<Vec<FilePatch>, String> {
    let mut patches = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut hunk: Option<Hunk> = None;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if let Some(old_path) = line.strip_prefix("--- ") {
            finish_hunk(&mut current, &mut hunk)?;
            finish_file(&mut patches, &mut current)?;
            index += 1;
            let new_line = lines
                .get(index)
                .ok_or_else(|| "unified diff is missing its +++ file header".to_string())?;
            let new_path = new_line
                .strip_prefix("+++ ")
                .ok_or_else(|| "unified diff is missing its +++ file header".to_string())?;
            let old_path = parse_diff_path(old_path)?;
            let new_path = parse_diff_path(new_path)?;
            let (operation, path, destination) = match (old_path, new_path) {
                (None, Some(path)) => (FileOperation::Add, path, None),
                (Some(path), None) => (FileOperation::Delete, path, None),
                (Some(old), Some(new)) if old == new => (FileOperation::Update, old, None),
                (Some(old), Some(new)) => (FileOperation::Move, old, Some(new)),
                (None, None) => return Err("unified diff has no file path".into()),
            };
            current = Some(FilePatch {
                operation,
                path,
                destination,
                hunks: Vec::new(),
            });
            index += 1;
            continue;
        }
        if line.starts_with("@@") {
            finish_hunk(&mut current, &mut hunk)?;
            if current.is_none() {
                return Err("patch hunk appears before a file header".into());
            }
            hunk = Some(parse_hunk_header(line)?);
        } else if let Some(hunk) = hunk.as_mut() {
            if line != "\\ No newline at end of file" {
                hunk.lines.push(parse_patch_line(line)?);
            }
        }
        index += 1;
    }

    finish_hunk(&mut current, &mut hunk)?;
    finish_file(&mut patches, &mut current)?;
    if patches.is_empty() {
        return Err("patch did not contain any unified diff file headers".into());
    }
    Ok(patches)
}

fn parse_diff_path(raw_path: &str) -> std::result::Result<Option<String>, String> {
    let path = raw_path
        .split_once('\t')
        .map(|(path, _)| path)
        .unwrap_or_else(|| raw_path.split_whitespace().next().unwrap_or(raw_path))
        .trim_matches('"');
    if path == "/dev/null" {
        return Ok(None);
    }
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    Ok(Some(path.into()))
}

fn parse_hunk_header(line: &str) -> std::result::Result<Hunk, String> {
    if line.trim() == "@@" {
        return Ok(Hunk {
            old_start: 1,
            lines: Vec::new(),
        });
    }
    let body = line
        .strip_prefix("@@")
        .and_then(|value| value.split_once("@@"))
        .map(|(ranges, _)| ranges.trim())
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    let old_start = body
        .split_whitespace()
        .find_map(|range| range.strip_prefix('-'))
        .and_then(|range| {
            range
                .split_once(',')
                .map(|(start, _)| start)
                .or(Some(range))
        })
        .and_then(|start| start.parse::<usize>().ok())
        .ok_or_else(|| format!("invalid old range in hunk header: {line}"))?;
    Ok(Hunk {
        old_start,
        lines: Vec::new(),
    })
}

fn parse_patch_line(line: &str) -> std::result::Result<PatchLine, String> {
    let (prefix, content) = line.split_at(1.min(line.len()));
    match prefix {
        " " => Ok(PatchLine::Context(content.into())),
        "+" => Ok(PatchLine::Add(content.into())),
        "-" => Ok(PatchLine::Remove(content.into())),
        _ => Err(format!("invalid line in patch hunk: {line}")),
    }
}

fn finish_hunk(
    current: &mut Option<FilePatch>,
    hunk: &mut Option<Hunk>,
) -> std::result::Result<(), String> {
    if let Some(hunk) = hunk.take() {
        if hunk.lines.is_empty() {
            return Err("patch hunk cannot be empty".into());
        }
        current
            .as_mut()
            .ok_or_else(|| "patch hunk has no file section".to_string())?
            .hunks
            .push(hunk);
    }
    Ok(())
}

fn finish_file(
    patches: &mut Vec<FilePatch>,
    current: &mut Option<FilePatch>,
) -> std::result::Result<(), String> {
    if let Some(file) = current.take() {
        if file.path.trim().is_empty() {
            return Err("patch contains an empty file path".into());
        }
        patches.push(file);
    }
    Ok(())
}

fn render_added_file(hunks: &[Hunk]) -> std::result::Result<String, String> {
    let mut lines = Vec::new();
    for hunk in hunks {
        for line in &hunk.lines {
            match line {
                PatchLine::Add(content) => lines.push(content.clone()),
                PatchLine::Context(_) | PatchLine::Remove(_) => {
                    return Err("add-file patches may only contain + lines".into())
                }
            }
        }
    }
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", lines.join("\n")))
    }
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> std::result::Result<String, String> {
    let trailing_newline = original.ends_with('\n');
    let mut lines = split_lines(original);

    for hunk in hunks {
        let expected = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(content) | PatchLine::Remove(content) => Some(content),
                PatchLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let preferred = hunk.old_start.saturating_sub(1).min(lines.len());
        let start = find_hunk_start(&lines, &expected, preferred)
            .ok_or_else(|| format!("hunk could not be applied near line {}", hunk.old_start))?;
        let end = start.saturating_add(expected.len());
        if end > lines.len() {
            return Err(format!(
                "hunk exceeds the current file near line {}",
                hunk.old_start
            ));
        }
        let replacement = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(content) | PatchLine::Add(content) => Some(content.clone()),
                PatchLine::Remove(_) => None,
            })
            .collect::<Vec<_>>();
        lines.splice(start..end, replacement);
    }

    let mut result = lines.join("\n");
    if trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

fn split_lines(content: &str) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn find_hunk_start(lines: &[String], expected: &[&String], preferred: usize) -> Option<usize> {
    if expected.is_empty() {
        return Some(preferred.min(lines.len()));
    }
    let matches_at = |start: usize| {
        start.saturating_add(expected.len()) <= lines.len()
            && lines[start..start + expected.len()]
                .iter()
                .zip(expected.iter())
                .all(|(actual, expected)| actual == *expected)
    };
    (preferred..lines.len())
        .find(|start| matches_at(*start))
        .or_else(|| (0..preferred).find(|start| matches_at(*start)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn parses_and_applies_agent_update_patch() {
        let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n }\n*** End Patch";
        let parsed = parse_patch(patch).unwrap();
        let result =
            apply_hunks("fn main() {\n    println!(\"old\");\n}\n", &parsed[0].hunks).unwrap();
        assert_eq!(result, "fn main() {\n    println!(\"new\");\n}\n");
    }

    #[test]
    fn renders_added_files_with_a_trailing_newline() {
        let parsed =
            parse_patch("*** Begin Patch\n*** Add File: notes.txt\n+first\n+second\n*** End Patch")
                .unwrap();
        assert_eq!(
            render_added_file(&parsed[0].hunks).unwrap(),
            "first\nsecond\n"
        );
    }

    #[test]
    fn parses_standard_unified_diff_headers() {
        let parsed =
            parse_patch("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new").unwrap();
        assert_eq!(parsed[0].operation, FileOperation::Update);
        assert_eq!(parsed[0].path, "src/lib.rs");
    }

    #[test]
    fn resolves_paths_outside_the_workspace() {
        let workspace = Path::new("/tmp/project/workspace");
        assert_eq!(
            resolve_patch_path(workspace, "../../secret.txt").unwrap(),
            Path::new("/tmp/project/workspace/../../secret.txt")
        );
        assert_eq!(
            resolve_patch_path(workspace, "/tmp/shared/secret.txt").unwrap(),
            Path::new("/tmp/shared/secret.txt")
        );
    }

    #[tokio::test]
    async fn applies_a_patch_through_the_tool() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("message.txt");
        tokio::fs::write(&file, "before\n").await.unwrap();
        let context = ToolContext {
            working_dir: directory.path().to_path_buf(),
            session_id: "test".into(),
            agent_id: "test".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(crate::tool::ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        };
        let result = PatchTool
            .execute(
                &context,
                &json!({
                    "patch": "*** Begin Patch\n*** Update File: message.txt\n@@\n-before\n+after\n*** End Patch"
                }),
            )
            .await
            .unwrap();
        assert_eq!(result.content, "Patch applied successfully to 1 file(s).");
        assert_eq!(tokio::fs::read_to_string(file).await.unwrap(), "after\n");
    }

    #[tokio::test]
    async fn applies_a_patch_outside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let external = directory.path().join("shared/message.txt");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(external.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&external, "before\n").await.unwrap();
        let context = ToolContext {
            working_dir: workspace,
            session_id: "test".into(),
            agent_id: "test".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(crate::tool::ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        };

        PatchTool
            .execute(
                &context,
                &json!({
                    "patch": "*** Begin Patch\n*** Update File: ../shared/message.txt\n@@\n-before\n+after\n*** End Patch"
                }),
            )
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(external).await.unwrap(),
            "after\n"
        );
    }
}
