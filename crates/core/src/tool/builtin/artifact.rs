use super::resolve_file_path;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::{now, WorkArtifact, WorkArtifactKind, WorkDatabase};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

const MAX_TITLE_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 500;

pub struct PublishArtifactTool {
    database: Arc<WorkDatabase>,
}

impl PublishArtifactTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArtifactParams {
    path: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    open: bool,
}

#[async_trait]
impl Tool for PublishArtifactTool {
    fn name(&self) -> &str {
        "publish_artifact"
    }

    fn description(&self) -> &str {
        "Publish a generated or updated file so the user can preview it in the artifact viewer. Call this after creating a user-facing file such as a markdown report, image, compiled LaTeX PDF, source file, or any other deliverable. Do not paste the whole file into the conversation. The file must exist before publishing."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Existing file path. Relative paths use the conversation's current directory; absolute paths are allowed."
                },
                "title": {
                    "type": "string",
                    "description": "Short title shown in the artifact card and viewer. Defaults to the file name."
                },
                "kind": {
                    "type": "string",
                    "enum": ["auto", "markdown", "pdf", "image", "latex", "text", "other"],
                    "description": "Preview kind. Use auto to infer it from the file extension."
                },
                "description": {
                    "type": "string",
                    "description": "Optional one-line context for the artifact."
                },
                "open": {
                    "type": "boolean",
                    "description": "Open the artifact viewer immediately. Use it only for the main deliverable; default false."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: PublishArtifactParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let raw_path = params.path.trim();
        if raw_path.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "path cannot be empty".into(),
            });
        }
        let requested = resolve_file_path(&ctx.current_dir(), raw_path);
        let metadata =
            tokio::fs::metadata(&requested)
                .await
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!("Could not inspect '{}': {error}", requested.display()),
                })?;
        if !metadata.is_file() {
            return Err(ToolError::Execution {
                tool: self.name().into(),
                message: format!("'{}' is not a file", requested.display()),
            });
        }
        let path = requested.canonicalize().unwrap_or(requested);
        let kind = parse_kind(params.kind.as_deref(), &path)?;
        let default_title = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Artifact");
        let title = params
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(default_title)
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect::<String>();
        let description = params
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(|description| {
                description
                    .chars()
                    .take(MAX_DESCRIPTION_CHARS)
                    .collect::<String>()
            });
        let (conversation_id, _) = ctx.tool_activation.work_scope(&ctx.session_id, "");
        let existing = self
            .database
            .artifact_by_path(&conversation_id, &path)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let timestamp = now();
        let artifact = WorkArtifact {
            id: existing
                .as_ref()
                .map(|artifact| artifact.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
            path: path.clone(),
            title: title.clone(),
            kind,
            description,
            created_at: existing
                .as_ref()
                .map(|artifact| artifact.created_at)
                .unwrap_or(timestamp),
            updated_at: timestamp,
        };
        let stored = self
            .database
            .upsert_artifact(&conversation_id, &artifact)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        Ok(ToolResult::ok(format!(
            "Artifact '{}' is ready at '{}'",
            stored.title,
            stored.path.display()
        ))
        .with_metadata(json!({
            "artifact": stored,
            "open": params.open
        })))
    }
}

fn parse_kind(value: Option<&str>, path: &std::path::Path) -> Result<WorkArtifactKind> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("auto") => Ok(WorkArtifactKind::from_path(path)),
        Some("markdown") => Ok(WorkArtifactKind::Markdown),
        Some("pdf") => Ok(WorkArtifactKind::Pdf),
        Some("image") => Ok(WorkArtifactKind::Image),
        Some("latex") => Ok(WorkArtifactKind::Latex),
        Some("text") => Ok(WorkArtifactKind::Text),
        Some("other") => Ok(WorkArtifactKind::Other),
        Some(other) => Err(ToolError::InvalidParams {
            tool: "publish_artifact".into(),
            message: format!("unknown artifact kind '{other}'"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::PublishArtifactTool;
    use crate::connection::SessionBinding;
    use crate::tool::{Tool, ToolActivation, ToolContext};
    use crate::work::{now, WorkArtifactKind, WorkConversation, WorkDatabase};
    use std::sync::Arc;

    fn context(root: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: root.to_path_buf(),
            workspace_root: root.to_path_buf(),
            session_id: "artifact-session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            context_controller: Arc::new(crate::agent::ContextController::for_test(
                100_000, 16_384,
            )),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    fn database(root: &std::path::Path) -> Arc<WorkDatabase> {
        let database = WorkDatabase::open_at(root.join("work.db")).unwrap();
        let timestamp = now();
        database
            .save_conversation(&WorkConversation {
                id: "artifact-session".into(),
                title: "Artifacts".into(),
                project_id: None,
                pinned: false,
                unread: false,
                is_private: false,
                created_at: timestamp,
                updated_at: timestamp,
                binding: SessionBinding::default(),
                context_summary: None,
                context_usage: crate::agent::ContextUsage::default(),
                messages: Vec::new(),
                checkpoints: Vec::new(),
                tasks: Vec::new(),
                sources: Vec::new(),
                agent_threads: Vec::new(),
                agent_thread_transcripts: std::collections::HashMap::new(),
                active_context: Vec::new(),
                active_window_id: "initial".into(),
                history_entries: Vec::new(),
            })
            .unwrap();
        database
    }

    #[tokio::test]
    async fn publishes_and_updates_an_artifact_by_path() {
        let directory = tempfile::tempdir().unwrap();
        let database = database(directory.path());
        let tool = PublishArtifactTool::new(database.clone());
        let path = directory.path().join("report.md");
        std::fs::write(&path, "# Report\n").unwrap();
        let ctx = context(directory.path());

        let first = tool
            .execute(
                &ctx,
                &serde_json::json!({ "path": "report.md", "title": "Report" }),
            )
            .await
            .unwrap();
        let first_metadata = first.metadata.unwrap();
        let first_artifact = first_metadata["artifact"].clone();
        assert_eq!(first_artifact["kind"], "markdown");
        assert_eq!(first_artifact["title"], "Report");

        let second = tool
            .execute(
                &ctx,
                &serde_json::json!({
                    "path": "report.md",
                    "title": "Updated report",
                    "open": true
                }),
            )
            .await
            .unwrap();
        let second_metadata = second.metadata.unwrap();
        let second_artifact = second_metadata["artifact"].clone();
        assert_eq!(second_artifact["id"], first_artifact["id"]);
        assert_eq!(second_artifact["title"], "Updated report");
        assert_eq!(second_metadata["open"], true);

        let stored = database.artifacts("artifact-session").unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].title, "Updated report");
        assert_eq!(stored[0].kind, WorkArtifactKind::Markdown);
    }

    #[tokio::test]
    async fn rejects_missing_files_and_directories() {
        let directory = tempfile::tempdir().unwrap();
        let database = database(directory.path());
        let tool = PublishArtifactTool::new(database);
        let ctx = context(directory.path());

        assert!(tool
            .execute(&ctx, &serde_json::json!({ "path": "missing.pdf" }))
            .await
            .is_err());
        assert!(tool
            .execute(&ctx, &serde_json::json!({ "path": "." }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rejects_unknown_kinds() {
        let directory = tempfile::tempdir().unwrap();
        let database = database(directory.path());
        let tool = PublishArtifactTool::new(database);
        std::fs::write(directory.path().join("report.pdf"), b"%PDF-1.4").unwrap();
        let ctx = context(directory.path());

        assert!(tool
            .execute(
                &ctx,
                &serde_json::json!({ "path": "report.pdf", "kind": "spreadsheet" })
            )
            .await
            .is_err());
    }

    #[test]
    fn infers_kinds_from_extensions() {
        assert_eq!(
            WorkArtifactKind::from_path(std::path::Path::new("report.md")),
            WorkArtifactKind::Markdown
        );
        assert_eq!(
            WorkArtifactKind::from_path(std::path::Path::new("report.PDF")),
            WorkArtifactKind::Pdf
        );
        assert_eq!(
            WorkArtifactKind::from_path(std::path::Path::new("photo.jpeg")),
            WorkArtifactKind::Image
        );
        assert_eq!(
            WorkArtifactKind::from_path(std::path::Path::new("paper.tex")),
            WorkArtifactKind::Latex
        );
        assert_eq!(
            WorkArtifactKind::from_path(std::path::Path::new("data.json")),
            WorkArtifactKind::Text
        );
        assert_eq!(
            WorkArtifactKind::from_path(std::path::Path::new("archive.zip")),
            WorkArtifactKind::Other
        );
    }
}
