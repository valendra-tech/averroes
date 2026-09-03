use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

/// Applies compact unified diffs, including the `*** Begin Patch` format used
/// by coding agents. The tool keeps the patch itself as the only large input
/// and returns a short result instead of echoing changed file contents.
pub struct PatchTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchParams {
    patch: String,
}

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
    change_context: Option<String>,
    lines: Vec<PatchLine>,
    is_end_of_file: bool,
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
    original: Option<Vec<u8>>,
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
                    "maxLength": 10485760,
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
        let params: PatchParams = serde_json::from_value(params.clone())
            .map_err(|error| invalid_params(error.to_string()))?;
        let patch = params.patch.as_str();
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

        apply_changes(&changes)
            .await
            .map_err(|message| ToolError::Execution {
                tool: self.name().into(),
                message,
            })?;

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
        match file_patch.operation {
            FileOperation::Add => {
                if source_exists {
                    return Err(format!(
                        "file to add already exists: {}",
                        destination.display()
                    ));
                }
                let content = render_added_file(&file_patch.hunks)?;
                changes.push(PreparedChange {
                    operation: FileOperation::Add,
                    source: destination.clone(),
                    destination,
                    content: Some(content),
                    original: None,
                });
            }
            FileOperation::Update | FileOperation::Move => {
                if !source_exists {
                    return Err(format!(
                        "file to update was not found: {}",
                        source.display()
                    ));
                }
                let original = tokio::fs::read(&source)
                    .await
                    .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
                let original_text = String::from_utf8(original.clone())
                    .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
                if file_patch.operation == FileOperation::Move
                    && destination != source
                    && tokio::fs::try_exists(&destination).await.map_err(|error| {
                        format!("cannot inspect '{}': {error}", destination.display())
                    })?
                {
                    return Err(format!(
                        "move destination already exists: {}",
                        destination.display()
                    ));
                }
                let content = apply_hunks(&original_text, &file_patch.hunks)?;
                changes.push(PreparedChange {
                    operation: file_patch.operation,
                    source,
                    destination,
                    content: Some(content),
                    original: Some(original),
                });
            }
            FileOperation::Delete => {
                if !source_exists {
                    return Err(format!(
                        "file to delete was not found: {}",
                        source.display()
                    ));
                }
                let original = tokio::fs::read(&source)
                    .await
                    .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
                if !file_patch.hunks.is_empty() {
                    let original_text = String::from_utf8(original.clone())
                        .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
                    let remaining = apply_hunks(&original_text, &file_patch.hunks)?;
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
                    original: Some(original),
                });
            }
        }
    }

    Ok(changes)
}

async fn apply_changes(changes: &[PreparedChange]) -> std::result::Result<(), String> {
    for (index, change) in changes.iter().enumerate() {
        if let Err(error) = apply_change(change).await {
            let rollback = rollback_changes(&changes[..=index]).await;
            return Err(match rollback {
                Ok(()) => format!("{error}; all changes were rolled back"),
                Err(rollback_error) => {
                    format!("{error}; rollback was incomplete: {rollback_error}")
                }
            });
        }
    }
    Ok(())
}

async fn apply_change(change: &PreparedChange) -> std::result::Result<(), String> {
    match change.operation {
        FileOperation::Delete => tokio::fs::remove_file(&change.source)
            .await
            .map_err(|error| execution_error(&change.source, error).to_string()),
        FileOperation::Add | FileOperation::Update | FileOperation::Move => {
            if let Some(parent) = change.destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| execution_error(parent, error).to_string())?;
            }
            tokio::fs::write(
                &change.destination,
                change.content.as_deref().unwrap_or_default(),
            )
            .await
            .map_err(|error| execution_error(&change.destination, error).to_string())?;

            if change.operation == FileOperation::Move {
                tokio::fs::remove_file(&change.source)
                    .await
                    .map_err(|error| execution_error(&change.source, error).to_string())?;
            }
            Ok(())
        }
    }
}

async fn rollback_changes(changes: &[PreparedChange]) -> std::result::Result<(), String> {
    let mut errors = Vec::new();
    for change in changes.iter().rev() {
        let result = match change.operation {
            FileOperation::Add => remove_file_if_exists(&change.destination).await,
            FileOperation::Update | FileOperation::Delete => {
                restore_file(
                    &change.destination,
                    change.original.as_deref().unwrap_or_default(),
                )
                .await
            }
            FileOperation::Move => {
                let remove_destination = remove_file_if_exists(&change.destination).await;
                let restore_source = restore_file(
                    &change.source,
                    change.original.as_deref().unwrap_or_default(),
                )
                .await;
                combine_rollback_results(remove_destination, restore_source)
            }
        };
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn restore_file(path: &Path, content: &[u8]) -> std::result::Result<(), String> {
    tokio::fs::write(path, content)
        .await
        .map_err(|error| format!("could not restore '{}': {error}", path.display()))
}

async fn remove_file_if_exists(path: &Path) -> std::result::Result<(), String> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(format!("could not remove '{}': {error}", path.display())),
    }
}

fn combine_rollback_results(
    first: std::result::Result<(), String>,
    second: std::result::Result<(), String>,
) -> std::result::Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(first), Ok(())) => Err(first),
        (Ok(()), Err(second)) => Err(second),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

fn resolve_patch_path(workspace: &Path, raw_path: &str) -> std::result::Result<PathBuf, String> {
    let path = raw_path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("patch contains an empty file path".into());
    }
    Ok(resolve_file_path(workspace, path))
}

fn parse_patch(input: &str) -> std::result::Result<Vec<FilePatch>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("patch cannot be empty".into());
    }
    let lines = input
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| line.trim() == "*** Begin Patch")
    {
        parse_agent_patch(&lines)
    } else {
        parse_unified_diff(&lines)
    }
}

fn parse_agent_patch(lines: &[&str]) -> std::result::Result<Vec<FilePatch>, String> {
    if lines.first().map(|line| line.trim()) != Some("*** Begin Patch") {
        return Err("the first line of the patch must be '*** Begin Patch'".into());
    }
    if lines.last().map(|line| line.trim()) != Some("*** End Patch") {
        return Err("the last line of the patch must be '*** End Patch'".into());
    }

    let mut patches = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut hunk: Option<Hunk> = None;

    for line in lines.iter().skip(1).take(lines.len().saturating_sub(2)) {
        let marker = (!line.starts_with(' ')).then(|| line.trim());

        if let Some(path) = marker.and_then(|line| line.strip_prefix("*** Add File: ")) {
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
        if let Some(path) = marker.and_then(|line| line.strip_prefix("*** Delete File: ")) {
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
        if let Some(path) = marker.and_then(|line| line.strip_prefix("*** Update File: ")) {
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
        if let Some(path) = marker.and_then(|line| line.strip_prefix("*** Move to: ")) {
            let file = current
                .as_mut()
                .ok_or_else(|| "*** Move to: must follow a file section".to_string())?;
            if file.operation != FileOperation::Update
                || !file.hunks.is_empty()
                || hunk.is_some()
                || file.destination.is_some()
            {
                return Err(
                    "*** Move to: is only valid before the update hunks and after *** Update File:"
                        .into(),
                );
            }
            file.operation = FileOperation::Move;
            file.destination = Some(path.trim().into());
            continue;
        }
        if marker == Some("*** End of File") {
            let file = current
                .as_ref()
                .ok_or_else(|| "*** End of File must follow a file section".to_string())?;
            if file.operation != FileOperation::Update && file.operation != FileOperation::Move {
                return Err("*** End of File is only valid in an update hunk".into());
            }
            let current_hunk = hunk
                .as_mut()
                .ok_or_else(|| "*** End of File must follow an update hunk".to_string())?;
            if current_hunk.lines.is_empty() {
                return Err("update hunk cannot be empty".into());
            }
            current_hunk.is_end_of_file = true;
            continue;
        }
        if line.starts_with("@@") {
            finish_hunk(&mut current, &mut hunk)?;
            if current.as_ref().is_none_or(|file| {
                file.operation == FileOperation::Add || file.operation == FileOperation::Delete
            }) {
                return Err("patch hunk appears before a file section".into());
            }
            hunk = Some(parse_hunk_header(line)?);
            continue;
        }
        if *line == "\\ No newline at end of file" {
            continue;
        }
        if let Some(file) = current.as_ref() {
            if file.operation == FileOperation::Add && hunk.is_none() && line.starts_with('+') {
                hunk = Some(Hunk {
                    old_start: 0,
                    change_context: None,
                    lines: Vec::new(),
                    is_end_of_file: false,
                });
            }
            if (file.operation == FileOperation::Update || file.operation == FileOperation::Move)
                && hunk.is_none()
                && (line.is_empty()
                    || line.starts_with(' ')
                    || line.starts_with('+')
                    || line.starts_with('-'))
            {
                hunk = Some(Hunk {
                    old_start: 1,
                    change_context: None,
                    lines: Vec::new(),
                    is_end_of_file: false,
                });
            }
        }
        if hunk
            .as_ref()
            .is_some_and(|current_hunk| current_hunk.is_end_of_file && line.trim().is_empty())
        {
            continue;
        }
        if line.is_empty() {
            if let Some(current_hunk) = hunk.as_mut() {
                current_hunk.lines.push(PatchLine::Context(String::new()));
                continue;
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
    let body = line
        .strip_prefix("@@")
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    let (ranges, context) = match body.split_once("@@") {
        Some((ranges, context)) => (ranges.trim(), context.trim()),
        None => ("", body.trim()),
    };
    let old_start = ranges
        .split_whitespace()
        .find_map(|range| range.strip_prefix('-'))
        .and_then(|range| {
            range
                .split_once(',')
                .map(|(start, _)| start)
                .or(Some(range))
        })
        .and_then(|start| start.parse::<usize>().ok())
        .unwrap_or(1);
    Ok(Hunk {
        old_start,
        change_context: (!context.is_empty()).then(|| context.to_string()),
        lines: Vec::new(),
        is_end_of_file: false,
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
        if (file.operation == FileOperation::Update || file.operation == FileOperation::Move)
            && file.hunks.is_empty()
        {
            return Err(format!(
                "update file hunk for path '{}' is empty",
                file.path.trim()
            ));
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
    if hunks.is_empty() {
        return Err("update patch did not contain any hunks".into());
    }

    let lines = split_lines(original);
    let mut replacements = Vec::with_capacity(hunks.len());
    let mut line_index = 0;

    for hunk in hunks {
        if let Some(context) = &hunk.change_context {
            let context_pattern = vec![context.clone()];
            let context_index = find_hunk_start(&lines, &context_pattern, line_index, false)
                .ok_or_else(|| {
                    format!("context could not be found near line {}", hunk.old_start)
                })?;
            line_index = context_index + 1;
        }

        let expected = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(content) | PatchLine::Remove(content) => Some(content.clone()),
                PatchLine::Add(_) => None,
            })
            .collect::<Vec<_>>();

        let replacement = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(content) | PatchLine::Add(content) => Some(content.clone()),
                PatchLine::Remove(_) => None,
            })
            .collect::<Vec<_>>();

        if expected.is_empty() {
            replacements.push((lines.len(), 0, replacement));
            continue;
        }

        let mut pattern = expected.as_slice();
        let mut new_lines = replacement.as_slice();
        let mut start = find_hunk_start(&lines, pattern, line_index, hunk.is_end_of_file);

        if start.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_lines.last().is_some_and(String::is_empty) {
                new_lines = &new_lines[..new_lines.len() - 1];
            }
            start = find_hunk_start(&lines, pattern, line_index, hunk.is_end_of_file);
        }

        let start = start
            .ok_or_else(|| format!("hunk could not be applied near line {}", hunk.old_start))?;
        if !hunk.is_end_of_file && start < line_index {
            return Err(format!(
                "hunks are out of order near line {}",
                hunk.old_start
            ));
        }
        replacements.push((start, pattern.len(), new_lines.to_vec()));
        line_index = start + pattern.len();
    }

    replacements.sort_by_key(|(start, _, _)| *start);
    for pair in replacements.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.0 + previous.1 > current.0 {
            return Err("patch contains overlapping hunks".into());
        }
    }

    let mut result = apply_replacements(lines, &replacements).join("\n");
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn split_lines(content: &str) -> Vec<String> {
    let mut lines = content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start, old_len, new_lines) in replacements.iter().rev() {
        lines.splice(*start..start.saturating_add(*old_len), new_lines.clone());
    }
    lines
}

fn find_hunk_start(
    lines: &[String],
    expected: &[String],
    preferred: usize,
    end_of_file: bool,
) -> Option<usize> {
    if expected.is_empty() {
        return Some(preferred.min(lines.len()));
    }
    if expected.len() > lines.len() {
        return None;
    }

    let last_start = lines.len() - expected.len();
    let search_start = preferred.min(last_start);
    let mut candidates = Vec::with_capacity(last_start - search_start + 1);
    if end_of_file {
        candidates.push(last_start);
    }
    candidates.extend(search_start..=last_start);

    for matching_mode in 0..4 {
        for start in &candidates {
            if lines[*start..*start + expected.len()]
                .iter()
                .zip(expected.iter())
                .all(|(actual, expected)| patch_lines_match(actual, expected, matching_mode))
            {
                return Some(*start);
            }
        }
    }
    None
}

fn patch_lines_match(actual: &str, expected: &str, matching_mode: usize) -> bool {
    match matching_mode {
        0 => actual == expected,
        1 => actual.trim_end() == expected.trim_end(),
        2 => actual.trim() == expected.trim(),
        _ => normalize_patch_line(actual) == normalize_patch_line(expected),
    }
}

fn normalize_patch_line(line: &str) -> String {
    line.trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
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
    fn parses_codex_update_context_and_end_of_file_marker() {
        let parsed = parse_patch(
            "*** Begin Patch\n*** Update File: src/main.rs\n@@ fn main()\n-old\n+new\n*** End of File\n*** End Patch",
        )
        .unwrap();

        assert_eq!(
            parsed[0].hunks[0].change_context.as_deref(),
            Some("fn main()")
        );
        assert!(parsed[0].hunks[0].is_end_of_file);
    }

    #[test]
    fn rejects_agent_patches_without_strict_boundaries() {
        assert!(parse_patch("*** Begin Patch\n*** Add File: file.txt\n+content").is_err());
        assert!(parse_patch("*** Begin Patch\n*** End Patch\nextra").is_err());
    }

    #[test]
    fn applies_chunks_in_original_file_order() {
        let patch = parse_patch(
            "*** Begin Patch\n*** Update File: file.txt\n@@\n a\n-b\n+B\n@@\n c\n-d\n+D\n*** End Patch",
        )
        .unwrap();

        let result = apply_hunks("a\nb\nc\nd\n", &patch[0].hunks).unwrap();
        assert_eq!(result, "a\nB\nc\nD\n");
    }

    #[test]
    fn appends_end_of_file_chunks() {
        let patch = parse_patch(
            "*** Begin Patch\n*** Update File: file.txt\n@@\n+b\n*** End of File\n*** End Patch",
        )
        .unwrap();

        let result = apply_hunks("a\n", &patch[0].hunks).unwrap();
        assert_eq!(result, "a\nb\n");
    }

    #[test]
    fn matches_context_with_codex_tolerance() {
        let patch = parse_patch(
            "*** Begin Patch\n*** Update File: file.txt\n@@\n-import asyncio  # local import - avoids top-level dep\n+changed\n*** End Patch",
        )
        .unwrap();
        let original = "import asyncio  # local import \u{2013} avoids top\u{2011}level dep\n";

        let result = apply_hunks(original, &patch[0].hunks).unwrap();
        assert_eq!(result, "changed\n");
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
    async fn rejects_invalid_parameter_shapes_before_touching_files() {
        let directory = tempfile::tempdir().unwrap();
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
        let invalid = [
            json!({"patch": ""}),
            json!({"patch": 42}),
            json!({"patch": "*** Begin Patch\n*** End Patch"}),
            json!({"patch": "*** Begin Patch\n*** End Patch", "unexpected": true}),
        ];

        for params in invalid {
            assert!(matches!(
                PatchTool.execute(&context, &params).await,
                Err(ToolError::InvalidParams { .. })
            ));
        }
        assert!(tokio::fs::read_dir(directory.path())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn rolls_back_earlier_changes_when_a_later_change_fails() {
        let directory = tempfile::tempdir().unwrap();
        let first_file = directory.path().join("first.txt");
        let blocked_parent = directory.path().join("blocked");
        tokio::fs::write(&first_file, "before\n").await.unwrap();
        tokio::fs::write(&blocked_parent, "not a directory\n")
            .await
            .unwrap();
        let changes = vec![
            PreparedChange {
                operation: FileOperation::Update,
                source: first_file.clone(),
                destination: first_file.clone(),
                content: Some("after\n".into()),
                original: Some(b"before\n".to_vec()),
            },
            PreparedChange {
                operation: FileOperation::Add,
                source: blocked_parent.join("second.txt"),
                destination: blocked_parent.join("second.txt"),
                content: Some("content\n".into()),
                original: None,
            },
        ];

        let error = apply_changes(&changes).await.unwrap_err();
        assert!(error.contains("all changes were rolled back"), "{error}");
        assert_eq!(
            tokio::fs::read_to_string(first_file).await.unwrap(),
            "before\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(blocked_parent).await.unwrap(),
            "not a directory\n"
        );
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_added_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("message.txt");
        tokio::fs::write(&file, "original\n").await.unwrap();
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
                    "patch": "*** Begin Patch\n*** Add File: message.txt\n+replacement\n*** End Patch"
                }),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(file).await.unwrap(), "original\n");
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
