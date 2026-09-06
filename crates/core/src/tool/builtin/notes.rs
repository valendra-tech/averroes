use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::{WorkDatabase, WorkNote};

pub struct NotesTool {
    database: Arc<WorkDatabase>,
}

impl NotesTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum NotesOperation {
    List,
    Read,
    Write,
    Append,
    Search,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotesParams {
    op: NotesOperation,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    offset: usize,
}

fn invalid(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParams {
        tool: tool.into(),
        message: message.into(),
    }
}

fn database_error(tool: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution {
        tool: tool.into(),
        message: error.to_string(),
    }
}

fn normalize_note_path(path: Option<&str>) -> std::result::Result<String, String> {
    let path = path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "path is required and cannot be empty".to_string())?;
    let path = Path::new(path);
    if path.is_absolute() || is_windows_absolute(path.to_string_lossy().as_ref()) {
        return Err("path must be relative to the note namespace".into());
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => return Err("path cannot contain '..' components".into()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("path must stay inside the relative note namespace".into())
            }
        }
    }

    if normalized.as_os_str().is_empty() || normalized.is_absolute() {
        return Err("path must name a note inside the relative note namespace".into());
    }
    Ok(normalized.to_string_lossy().into_owned())
}

fn is_windows_absolute(path: &str) -> bool {
    path.starts_with('\\')
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

fn required_content(params: &NotesParams, tool: &str, allow_empty: bool) -> Result<String> {
    let content = params
        .content
        .as_deref()
        .ok_or_else(|| invalid(tool, "content is required"))?;
    if !allow_empty && content.trim().is_empty() {
        return Err(invalid(tool, "content cannot be empty"));
    }
    Ok(content.to_owned())
}

fn note_path(params: &NotesParams, tool: &str) -> Result<String> {
    normalize_note_path(params.path.as_deref()).map_err(|message| invalid(tool, message))
}

fn workspace_root(ctx: &ToolContext, tool: &str) -> Result<String> {
    let root = ctx.workspace_root.to_string_lossy().trim().to_owned();
    if root.is_empty() {
        return Err(invalid(tool, "workspace_root is required"));
    }
    Ok(root)
}

fn char_offset(value: &str, offset: usize) -> usize {
    value
        .char_indices()
        .nth(offset)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn char_slice(value: &str, start: usize, end: usize) -> &str {
    &value[char_offset(value, start)..char_offset(value, end)]
}

fn note_summary(note: &WorkNote, query: &str) -> String {
    let query = query.to_lowercase();
    let excerpt = note
        .content
        .lines()
        .find(|line| line.to_lowercase().contains(&query))
        .or_else(|| note.content.lines().next())
        .unwrap_or_default();
    let excerpt = excerpt.chars().take(512).collect::<String>();
    if excerpt.is_empty() {
        note.path.clone()
    } else {
        format!("{}: {}", note.path, excerpt)
    }
}

#[async_trait]
impl Tool for NotesTool {
    fn name(&self) -> &str {
        "notes"
    }

    fn description(&self) -> &str {
        "Read and persist private SQLite-backed notes scoped to the current workspace. Use list, read, write, append, or search; note paths are always relative."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["list", "read", "write", "append", "search"],
                    "description": "The note operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Relative note path; required for read, write, and append"
                },
                "content": {
                    "type": "string",
                    "description": "Replacement content for write or one record for append"
                },
                "query": {
                    "type": "string",
                    "description": "Case-insensitive substring to find in note paths or content"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Character offset for a paginated read"
                }
            },
            "required": ["op"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: NotesParams = serde_json::from_value(params.clone())
            .map_err(|error| invalid(self.name(), error.to_string()))?;
        let workspace_root = workspace_root(ctx, self.name())?;

        match params.op {
            NotesOperation::List => {
                let notes = self
                    .database
                    .list_notes(&workspace_root)
                    .map_err(|error| database_error(self.name(), error))?;
                let paths = notes
                    .iter()
                    .map(|note| note.path.as_str())
                    .collect::<Vec<_>>();
                let content = if paths.is_empty() {
                    "No notes found in this workspace.".to_owned()
                } else {
                    paths.join("\n")
                };
                Ok(ToolResult::ok(content).with_metadata(json!({
                    "op": "list",
                    "paths": paths,
                    "count": notes.len(),
                })))
            }
            NotesOperation::Read => {
                let path = note_path(&params, self.name())?;
                let Some(note) = self
                    .database
                    .read_note(&workspace_root, &path)
                    .map_err(|error| database_error(self.name(), error))?
                else {
                    return Ok(ToolResult::error(format!("Note '{path}' does not exist.")));
                };
                let total = note.chars().count();
                if params.offset > total {
                    return Err(invalid(
                        self.name(),
                        format!(
                            "offset {} exceeds note length {total} for '{path}'",
                            params.offset
                        ),
                    ));
                }
                let page_size = ctx
                    .safe_page_chars(params.offset)
                    .map_err(|message| invalid(self.name(), message))?;
                let end = params.offset.saturating_add(page_size).min(total);
                let page = char_slice(&note, params.offset, end);
                let has_more = end < total;
                let continuation = has_more
                    .then(|| format!("; continue with offset {end}"))
                    .unwrap_or_default();
                let content = format!(
                    "[chars {}-{end} of {total}{continuation}]\n{page}",
                    params.offset
                );
                Ok(ToolResult::ok(content).with_metadata(json!({
                    "op": "read",
                    "path": path,
                    "offset": params.offset,
                    "start": params.offset,
                    "end": end,
                    "total": total,
                    "has_more": has_more,
                    "next_offset": has_more.then_some(end),
                })))
            }
            NotesOperation::Write => {
                let path = note_path(&params, self.name())?;
                let content = required_content(&params, self.name(), true)?;
                self.database
                    .write_note(&workspace_root, &path, &content)
                    .map_err(|error| database_error(self.name(), error))?;
                Ok(
                    ToolResult::ok(format!("Wrote note '{path}'.")).with_metadata(json!({
                        "op": "write",
                        "path": path,
                        "char_count": content.chars().count(),
                    })),
                )
            }
            NotesOperation::Append => {
                let path = note_path(&params, self.name())?;
                let content = required_content(&params, self.name(), false)?;
                self.database
                    .append_note(&workspace_root, &path, &content)
                    .map_err(|error| database_error(self.name(), error))?;
                Ok(
                    ToolResult::ok(format!("Appended to note '{path}'.")).with_metadata(json!({
                        "op": "append",
                        "path": path,
                        "char_count": content.chars().count(),
                    })),
                )
            }
            NotesOperation::Search => {
                let query = params
                    .query
                    .as_deref()
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or_else(|| invalid(self.name(), "query is required and cannot be empty"))?;
                let notes = self
                    .database
                    .search_notes(&workspace_root, query)
                    .map_err(|error| database_error(self.name(), error))?;
                let content = if notes.is_empty() {
                    format!("No notes matched '{query}'.")
                } else {
                    notes
                        .iter()
                        .map(|note| note_summary(note, query))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(ToolResult::ok(content).with_metadata(json!({
                    "op": "search",
                    "query": query,
                    "paths": notes.iter().map(|note| note.path.clone()).collect::<Vec<_>>(),
                    "count": notes.len(),
                })))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NotesTool;
    use crate::agent::ContextController;
    use crate::storage::work::WorkDatabase;
    use crate::tool::{Tool, ToolActivation, ToolContext, ToolError, ToolRegistry};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn context(root: &str) -> ToolContext {
        context_with_controller(root, Arc::new(ContextController::for_test(100_000, 16_384)))
    }

    fn context_with_controller(
        root: &str,
        context_controller: Arc<ContextController>,
    ) -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from(root),
            workspace_root: PathBuf::from(root),
            session_id: "notes-test-session".into(),
            agent_id: "notes-test-agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            context_controller,
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    fn database() -> (tempfile::TempDir, Arc<WorkDatabase>) {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        (directory, database)
    }

    #[tokio::test]
    async fn notes_support_list_read_write_append_and_search() {
        let (_directory, database) = database();
        let tool = NotesTool::new(database);
        let ctx = context("/workspace");

        tool.execute(
            &ctx,
            &json!({"op": "write", "path": "state.md", "content": "first draft"}),
        )
        .await
        .unwrap();
        tool.execute(
            &ctx,
            &json!({"op": "append", "path": "state.md", "content": "second line"}),
        )
        .await
        .unwrap();

        let listed = tool.execute(&ctx, &json!({"op": "list"})).await.unwrap();
        assert!(listed.success);
        assert!(listed.content.contains("state.md"));

        let read = tool
            .execute(&ctx, &json!({"op": "read", "path": "state.md"}))
            .await
            .unwrap();
        assert!(read.success);
        assert!(read.content.contains("first draft\nsecond line\n"));

        let search = tool
            .execute(&ctx, &json!({"op": "search", "query": "SECOND"}))
            .await
            .unwrap();
        assert!(search.success);
        assert!(search.content.contains("state.md"));
        assert!(search.content.contains("second line"));
    }

    #[tokio::test]
    async fn empty_write_clears_note_content() {
        let (_directory, database) = database();
        let tool = NotesTool::new(database);
        let ctx = context("/workspace");

        tool.execute(
            &ctx,
            &json!({"op": "write", "path": "state.md", "content": "content"}),
        )
        .await
        .unwrap();
        tool.execute(
            &ctx,
            &json!({"op": "write", "path": "state.md", "content": ""}),
        )
        .await
        .unwrap();

        let read = tool
            .execute(&ctx, &json!({"op": "read", "path": "state.md"}))
            .await
            .unwrap();
        assert!(read.success);
        assert!(read.content.contains("of 0"));
        assert!(!read.content.contains("content"));
    }

    #[tokio::test]
    async fn notes_reject_missing_required_parameters_and_unknown_fields() {
        let (_directory, database) = database();
        let tool = NotesTool::new(database);
        let ctx = context("/workspace");

        let invalid = [
            json!({}),
            json!({"op": "read"}),
            json!({"op": "write", "path": "state.md"}),
            json!({"op": "append", "path": "state.md"}),
            json!({"op": "append", "content": "record"}),
            json!({"op": "search"}),
            json!({"op": "list", "unexpected": true}),
        ];
        for params in invalid {
            assert!(matches!(
                tool.execute(&ctx, &params).await,
                Err(ToolError::InvalidParams { .. })
            ));
        }
    }

    #[tokio::test]
    async fn notes_reject_paths_outside_the_relative_note_namespace() {
        let (_directory, database) = database();
        let tool = NotesTool::new(database);
        let ctx = context("/workspace");

        for path in [
            "",
            ".",
            "/absolute.md",
            "C:\\absolute.md",
            "../escape.md",
            "a/../../escape.md",
        ] {
            let error = tool
                .execute(
                    &ctx,
                    &json!({"op": "write", "path": path, "content": "blocked"}),
                )
                .await
                .unwrap_err();
            assert!(matches!(error, ToolError::InvalidParams { .. }), "{path:?}");
        }

        tool.execute(
            &ctx,
            &json!({"op": "write", "path": "./state.md", "content": "safe"}),
        )
        .await
        .unwrap();
        let read = tool
            .execute(&ctx, &json!({"op": "read", "path": "state.md"}))
            .await
            .unwrap();
        assert!(read.content.contains("safe"));
    }

    #[tokio::test]
    async fn read_pages_content_and_continues_from_the_returned_offset() {
        let (_directory, database) = database();
        let context_controller = Arc::new(ContextController::for_test(10_000, 1_000));
        let page_size = context_controller.safe_page_chars(0, 0, 0, 0, 0).unwrap();
        let tool = NotesTool::new(database);
        let ctx = context_with_controller("/workspace", context_controller);
        let content = "x".repeat(page_size * 2 + 3);

        tool.execute(
            &ctx,
            &json!({"op": "write", "path": "large.md", "content": content}),
        )
        .await
        .unwrap();

        let first = tool
            .execute(&ctx, &json!({"op": "read", "path": "large.md"}))
            .await
            .unwrap();
        assert!(first
            .content
            .contains(&format!("chars 0-{page_size} of {}", page_size * 2 + 3)));
        assert_eq!(first.metadata.as_ref().unwrap()["next_offset"], page_size);

        let second = tool
            .execute(
                &ctx,
                &json!({"op": "read", "path": "large.md", "offset": page_size}),
            )
            .await
            .unwrap();
        assert!(second
            .content
            .contains(&format!("chars {page_size}-{}", page_size * 2)));
        assert_eq!(second.metadata.as_ref().unwrap()["offset"], page_size);
    }

    #[tokio::test]
    async fn too_small_read_pages_preserve_the_requested_offset_in_errors() {
        let (_directory, database) = database();
        let context_controller = Arc::new(ContextController::for_test(10_000, 9_500));
        let tool = NotesTool::new(database);
        let ctx = context_with_controller("/workspace", context_controller);

        tool.execute(
            &ctx,
            &json!({"op": "write", "path": "small.md", "content": "x".repeat(200)}),
        )
        .await
        .unwrap();
        let error = tool
            .execute(
                &ctx,
                &json!({"op": "read", "path": "small.md", "offset": 123}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("offset 123"));
    }

    #[tokio::test]
    async fn notes_are_isolated_by_workspace() {
        let (_directory, database) = database();
        let tool = NotesTool::new(database);
        let workspace_a = context("/workspace-a");
        let workspace_b = context("/workspace-b");

        tool.execute(
            &workspace_a,
            &json!({"op": "write", "path": "state.md", "content": "A"}),
        )
        .await
        .unwrap();

        let list_b = tool
            .execute(&workspace_b, &json!({"op": "list"}))
            .await
            .unwrap();
        assert!(!list_b.content.contains("state.md"));
        let read_b = tool
            .execute(&workspace_b, &json!({"op": "read", "path": "state.md"}))
            .await
            .unwrap();
        assert!(!read_b.success);
    }

    #[tokio::test]
    async fn concurrent_appends_preserve_complete_records() {
        let (_directory, database) = database();
        let second_database = WorkDatabase::open_at(database.path().to_path_buf()).unwrap();
        let first_tool = Arc::new(NotesTool::new(database.clone()));
        let second_tool = Arc::new(NotesTool::new(second_database));
        let ctx = context("/workspace");
        let mut tasks = Vec::new();
        for index in 0..16 {
            let tool = if index % 2 == 0 {
                first_tool.clone()
            } else {
                second_tool.clone()
            };
            let ctx = ctx.clone();
            tasks.push(tokio::spawn(async move {
                tool.execute(
                    &ctx,
                    &json!({
                        "op": "append",
                        "path": "events.md",
                        "content": format!("event-{index}"),
                    }),
                )
                .await
                .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let stored = database
            .read_note("/workspace", "events.md")
            .unwrap()
            .unwrap();
        let records = stored.lines().collect::<Vec<_>>();
        assert_eq!(records.len(), 16);
        for index in 0..16 {
            assert!(records.contains(&format!("event-{index}").as_str()));
        }
    }

    #[test]
    fn forked_registries_inherit_the_database_backed_notes_tool() {
        let (_directory, database) = database();
        let registry = ToolRegistry::new();
        registry.register(NotesTool::new(database));

        let scoped = registry.fork();

        assert_eq!(scoped.get("notes").unwrap().name(), "notes");
    }
}
