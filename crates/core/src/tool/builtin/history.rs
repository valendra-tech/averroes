use crate::storage::work::{WorkDatabase, WorkHistoryEntry, WorkHistoryKind};
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 50;
const MAX_SEARCH_OFFSET: usize = 100_000;
const READ_PAGE_CHARS: usize = 4_096;
const MAX_EXCERPT_CHARS: usize = 512;

fn canonical_workspace_root(ctx: &ToolContext) -> String {
    ctx.workspace_root
        .canonicalize()
        .unwrap_or_else(|_| ctx.workspace_root.clone())
        .to_string_lossy()
        .into_owned()
}

pub struct HistoryTool {
    database: Arc<WorkDatabase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchParams {
    operation: String,
    query: String,
    #[serde(default, deserialize_with = "reject_null_option")]
    limit: Option<usize>,
    #[serde(default, deserialize_with = "reject_null_option")]
    offset: Option<usize>,
    #[serde(default)]
    all: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadParams {
    operation: String,
    id: String,
    #[serde(default, deserialize_with = "reject_null_option")]
    offset: Option<usize>,
}

fn reject_null_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?
        .ok_or_else(|| D::Error::custom("must not be null"))
        .map(Some)
}

impl HistoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }

    fn invalid(&self, message: impl Into<String>) -> ToolError {
        ToolError::InvalidParams {
            tool: self.name().into(),
            message: message.into(),
        }
    }

    fn operation(params: &Value) -> Result<&str> {
        let object = params.as_object().ok_or_else(|| ToolError::InvalidParams {
            tool: "history".into(),
            message: "parameters must be an object".into(),
        })?;
        object
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidParams {
                tool: "history".into(),
                message: "operation is required and must be 'search' or 'read'".into(),
            })
    }

    async fn search(&self, ctx: &ToolContext, params: SearchParams) -> Result<ToolResult> {
        if params.operation != "search" {
            return Err(self.invalid("operation must be 'search'"));
        }
        let query = params.query.trim();
        if query.is_empty() {
            return Err(self.invalid("query is required and must not be empty"));
        }
        let limit = params.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
            return Err(self.invalid(format!("limit must be between 1 and {MAX_SEARCH_LIMIT}")));
        }
        let offset = params.offset.unwrap_or(0);
        if offset > MAX_SEARCH_OFFSET {
            return Err(self.invalid(format!("offset must be at most {MAX_SEARCH_OFFSET}")));
        }

        let scope_conversation = self.scope_conversation(ctx)?;
        let mut entries = if params.all {
            self.database
                .history_entries_for_workspace(&canonical_workspace_root(ctx))
                .map_err(|error| self.storage_error(error))?
        } else {
            self.database
                .history_entries(&scope_conversation)
                .map_err(|error| self.storage_error(error))?
                .into_iter()
                .map(|entry| (scope_conversation.clone(), entry))
                .collect()
        };
        let query = query.to_lowercase();
        entries.retain(|(_, entry)| entry.text.to_lowercase().contains(&query));

        entries.sort_by(|(left_conversation, left), (right_conversation, right)| {
            derived_rank(left)
                .cmp(&derived_rank(right))
                .then_with(|| kind_rank(&left.kind).cmp(&kind_rank(&right.kind)))
                .then_with(|| left.sequence.cmp(&right.sequence))
                .then_with(|| left.entry_id.cmp(&right.entry_id))
                .then_with(|| left_conversation.cmp(right_conversation))
        });

        let mut seen = HashSet::new();
        entries.retain(|(_, entry)| seen.insert(entry.entry_id.clone()));
        let total = entries.len();
        let page = entries
            .iter()
            .skip(offset)
            .take(limit)
            .map(|(conversation_id, entry)| format_search_entry(conversation_id, entry))
            .collect::<Vec<_>>();
        let has_more = total > offset.saturating_add(page.len());
        let next_offset = has_more.then_some(offset.saturating_add(page.len()));
        let content = if page.is_empty() {
            "No history entries matched the query.".into()
        } else {
            page.join("\n")
        };
        Ok(ToolResult::ok(content).with_metadata(json!({
            "offset": offset,
            "limit": limit,
            "total": total,
            "result_count": page.len(),
            "has_more": has_more,
            "next_offset": next_offset,
        })))
    }

    async fn read(&self, ctx: &ToolContext, params: ReadParams) -> Result<ToolResult> {
        if params.operation != "read" {
            return Err(self.invalid("operation must be 'read'"));
        }
        let id = params.id.trim();
        if id.is_empty() {
            return Err(self.invalid("id is required and must not be empty"));
        }
        let offset = params.offset.unwrap_or(0);
        let workspace_root = canonical_workspace_root(ctx);
        let scope_conversation = self.scope_conversation(ctx)?;
        let (_, entry) = self
            .database
            .history_entry_in_scope(&scope_conversation, &workspace_root, id)
            .map_err(|error| self.storage_error(error))?
            .ok_or_else(|| self.invalid(format!("history entry '{id}' was not found")))?;

        let total = entry.text.chars().count();
        if offset > total {
            return Err(self.invalid(format!(
                "offset {offset} exceeds history entry length {total}"
            )));
        }
        let start = offset;
        let end = (start + READ_PAGE_CHARS).min(total);
        let content = entry
            .text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect::<String>();
        let has_more = end < total;
        let next_offset = has_more.then_some(end);
        let mut result = ToolResult::ok(content).with_metadata(json!({
            "id": entry.entry_id,
            "kind": kind_name(&entry.kind),
            "offset": start,
            "start": start,
            "end": end,
            "total": total,
            "has_more": has_more,
            "next_offset": next_offset,
        }));
        if matches!(&entry.kind, WorkHistoryKind::ToolResult) {
            result.images = entry.images;
        }
        Ok(result)
    }

    fn storage_error(&self, error: impl std::fmt::Display) -> ToolError {
        ToolError::Execution {
            tool: self.name().into(),
            message: error.to_string(),
        }
    }

    fn scope_conversation(&self, ctx: &ToolContext) -> Result<String> {
        let workspace_root = canonical_workspace_root(ctx);
        self.database
            .resolve_history_conversation(&ctx.session_id, &workspace_root)
            .map_err(|error| self.storage_error(error))?
            .or_else(|| Some(ctx.session_id.clone()))
            .ok_or_else(|| self.invalid("conversation scope could not be resolved"))
    }
}

#[async_trait]
impl Tool for HistoryTool {
    fn name(&self) -> &str {
        "history"
    }

    fn description(&self) -> &str {
        "Search and read normalized conversation history from the current conversation or workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "operation": {"const": "search"},
                        "query": {"type": "string"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT},
                        "offset": {"type": "integer", "minimum": 0},
                        "all": {"type": "boolean", "default": false}
                    },
                    "required": ["operation", "query"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "operation": {"const": "read"},
                        "id": {"type": "string"},
                        "offset": {"type": "integer", "minimum": 0}
                    },
                    "required": ["operation", "id"],
                    "additionalProperties": false
                }
            ]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        match Self::operation(params)? {
            "search" => {
                let params = serde_json::from_value::<SearchParams>(params.clone())
                    .map_err(|error| self.invalid(error.to_string()))?;
                self.search(ctx, params).await
            }
            "read" => {
                let params = serde_json::from_value::<ReadParams>(params.clone())
                    .map_err(|error| self.invalid(error.to_string()))?;
                self.read(ctx, params).await
            }
            operation => Err(self.invalid(format!(
                "unknown operation '{operation}'; expected 'search' or 'read'"
            ))),
        }
    }
}

fn kind_rank(kind: &WorkHistoryKind) -> u8 {
    match kind {
        WorkHistoryKind::User => 0,
        WorkHistoryKind::Assistant => 1,
        WorkHistoryKind::ToolCall => 2,
        WorkHistoryKind::ToolResult => 3,
        WorkHistoryKind::ContextWindow => 4,
        WorkHistoryKind::Reminder => 5,
        WorkHistoryKind::Unknown(_) => 6,
    }
}

fn kind_name(kind: &WorkHistoryKind) -> String {
    match kind {
        WorkHistoryKind::User => "user",
        WorkHistoryKind::Assistant => "assistant",
        WorkHistoryKind::ToolCall => "tool_call",
        WorkHistoryKind::ToolResult => "tool_result",
        WorkHistoryKind::ContextWindow => "context_window",
        WorkHistoryKind::Reminder => "reminder",
        WorkHistoryKind::Unknown(value) => value,
    }
    .into()
}

fn derived_rank(entry: &WorkHistoryEntry) -> u8 {
    let payload = &entry.payload;
    let explicit = payload
        .get("derived")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || payload
            .get("echo")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let source = ["source", "origin", "tool"]
        .into_iter()
        .filter_map(|key| payload.get(key).and_then(Value::as_str))
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("history") || value.contains("note")
        });
    u8::from(explicit || source)
}

fn format_search_entry(conversation_id: &str, entry: &WorkHistoryEntry) -> String {
    let mut excerpt = entry
        .text
        .chars()
        .take(MAX_EXCERPT_CHARS)
        .collect::<String>();
    if entry.text.chars().count() > MAX_EXCERPT_CHARS {
        excerpt.push('…');
    }
    excerpt = excerpt.replace('\n', " ");
    if !entry.images.is_empty() {
        let mut media = HashMap::<&str, usize>::new();
        for image in &entry.images {
            *media.entry(image.media_type.as_str()).or_default() += 1;
        }
        let mut media = media.into_iter().collect::<Vec<_>>();
        media.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let summary = media
            .into_iter()
            .map(|(media_type, count)| format!("{count} {media_type}"))
            .collect::<Vec<_>>()
            .join(", ");
        excerpt.push_str(&format!(" [images: {summary}]"));
    }
    format!(
        "- `{}` [{}] (conversation `{}`): {}",
        entry.entry_id,
        kind_name(&entry.kind),
        conversation_id,
        excerpt
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::orchestration::{AgentThreadSnapshot, AgentThreadStatus};
    use crate::agent::{ContextBudget, ContextController};
    use crate::connection::SessionBinding;
    use crate::provider::types::ImageSource;
    use crate::storage::work::{
        now, WorkConversation, WorkDatabase, WorkHistoryEntry, WorkHistoryKind,
    };
    use crate::tool::{Tool, ToolActivation, ToolContext, ToolError, ToolRegistry};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    fn context(session_id: &str, root: &Path) -> ToolContext {
        ToolContext {
            working_dir: root.to_path_buf(),
            workspace_root: root.to_path_buf(),
            session_id: session_id.into(),
            agent_id: "agent-1".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            context_controller: Arc::new(ContextController::new(ContextBudget::new(
                128_000, 1_000, true,
            ))),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    fn conversation(id: &str, project_id: Option<String>) -> WorkConversation {
        WorkConversation {
            id: id.into(),
            title: id.into(),
            project_id,
            pinned: false,
            unread: false,
            created_at: now(),
            updated_at: now(),
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: Default::default(),
            messages: Vec::new(),
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: Vec::new(),
            agent_thread_transcripts: HashMap::new(),
            active_context: Vec::new(),
            active_window_id: "initial".into(),
            history_entries: Vec::new(),
        }
    }

    fn entry(id: &str, sequence: i64, kind: WorkHistoryKind, text: &str) -> WorkHistoryEntry {
        WorkHistoryEntry {
            entry_id: id.into(),
            parent_id: None,
            thread_id: None,
            window_id: "window-1".into(),
            sequence,
            timestamp: sequence,
            kind,
            text: text.into(),
            payload: json!({}),
            images: Vec::new(),
        }
    }

    fn database_with_workspace(
        root: &Path,
        conversations: &[WorkConversation],
    ) -> (tempfile::TempDir, Arc<WorkDatabase>) {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        let project = database.open_project(root).unwrap();
        for conversation in conversations {
            let mut conversation = conversation.clone();
            conversation.project_id = Some(project.id.clone());
            database.save_conversation(&conversation).unwrap();
        }
        (directory, database)
    }

    #[tokio::test]
    async fn search_defaults_to_current_conversation_and_all_searches_workspace() {
        let root = tempfile::tempdir().unwrap();
        let isolated_root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let other = conversation("other", None);
        let (directory, database) = database_with_workspace(root.path(), &[current, other]);
        let isolated_database =
            WorkDatabase::open_at(directory.path().join("isolated.db")).unwrap();
        let isolated_project = isolated_database
            .open_project(isolated_root.path())
            .unwrap();
        let isolated = conversation("isolated", Some(isolated_project.id));
        isolated_database.save_conversation(&isolated).unwrap();
        isolated_database
            .append_history_entries(
                "isolated",
                &[entry(
                    "isolated-entry",
                    1,
                    WorkHistoryKind::User,
                    "workspace needle",
                )],
            )
            .unwrap();
        database
            .append_history_entries(
                "current",
                &[entry(
                    "current-entry",
                    1,
                    WorkHistoryKind::User,
                    "workspace needle",
                )],
            )
            .unwrap();
        database
            .append_history_entries(
                "other",
                &[entry(
                    "other-entry",
                    1,
                    WorkHistoryKind::User,
                    "workspace needle",
                )],
            )
            .unwrap();

        let tool = HistoryTool::new(database);
        let ctx = context("current", root.path());
        let current_result = tool
            .execute(&ctx, &json!({"operation": "search", "query": "needle"}))
            .await
            .unwrap();
        assert!(current_result.content.contains("current-entry"));
        assert!(!current_result.content.contains("other-entry"));

        let all_result = tool
            .execute(
                &ctx,
                &json!({"operation": "search", "query": "needle", "all": true}),
            )
            .await
            .unwrap();
        assert!(all_result.content.contains("current-entry"));
        assert!(all_result.content.contains("other-entry"));
        assert!(!all_result.content.contains("isolated-entry"));
    }

    #[tokio::test]
    async fn search_ranks_original_content_and_kinds_before_history_echoes() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        let mut entries = vec![
            entry("reminder", 1, WorkHistoryKind::Reminder, "rank reminder"),
            entry("context", 2, WorkHistoryKind::ContextWindow, "rank context"),
            entry("tool-result", 3, WorkHistoryKind::ToolResult, "rank result"),
            entry("tool-call", 4, WorkHistoryKind::ToolCall, "rank call"),
            entry("assistant", 5, WorkHistoryKind::Assistant, "rank assistant"),
            entry("user", 6, WorkHistoryKind::User, "rank user"),
        ];
        let mut echo = entry(
            "history-echo",
            0,
            WorkHistoryKind::ToolResult,
            "rank history echo",
        );
        echo.payload = json!({"source": "history"});
        entries.push(echo);
        database
            .append_history_entries("current", &entries)
            .unwrap();

        let result = HistoryTool::new(database)
            .execute(
                &context("current", root.path()),
                &json!({"operation": "search", "query": "rank", "limit": 20}),
            )
            .await
            .unwrap();
        let position = |id: &str| result.content.find(id).unwrap();
        assert!(position("user") < position("assistant"));
        assert!(position("assistant") < position("tool-call"));
        assert!(position("tool-call") < position("tool-result"));
        assert!(position("tool-result") < position("context"));
        assert!(position("context") < position("reminder"));
        assert!(position("user") < position("history-echo"));
    }

    #[tokio::test]
    async fn search_deduplicates_entry_ids_across_workspace_views() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let delegated = conversation("delegated", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current, delegated]);
        let original = entry(
            "same-entry",
            1,
            WorkHistoryKind::User,
            "deduplicate this result",
        );
        let mut delegated_view = entry(
            "same-entry",
            1,
            WorkHistoryKind::ToolResult,
            "deduplicate this result",
        );
        delegated_view.thread_id = Some("thread-1".into());
        database
            .append_history_entries("current", &[original])
            .unwrap();
        database
            .append_history_entries("delegated", &[delegated_view])
            .unwrap();

        let result = HistoryTool::new(database)
            .execute(
                &context("current", root.path()),
                &json!({"operation": "search", "query": "deduplicate", "all": true}),
            )
            .await
            .unwrap();
        assert_eq!(result.content.matches("same-entry").count(), 1);
    }

    #[tokio::test]
    async fn delegated_session_resolves_parent_scope_and_keeps_workspace_isolation() {
        let root = tempfile::tempdir().unwrap();
        let isolated_root = tempfile::tempdir().unwrap();
        let mut parent = conversation("parent", None);
        parent.agent_threads = vec![AgentThreadSnapshot {
            id: "thread-1".into(),
            thread_id: "thread-1".into(),
            agent_id: "researcher".into(),
            parent_session_id: "parent".into(),
            title: "Research".into(),
            model_id: "model-1".into(),
            status: AgentThreadStatus::Completed,
            enabled_tools: vec!["history".into()],
            prompt: "Find it".into(),
            output: "Done".into(),
            created_at: 1,
            updated_at: 2,
        }];
        let (_directory, database) = database_with_workspace(root.path(), &[parent]);
        database
            .append_history_entries(
                "parent",
                &[
                    entry(
                        "parent-entry",
                        1,
                        WorkHistoryKind::User,
                        "parent delegated needle",
                    ),
                    {
                        let mut delegated = entry(
                            "delegated-entry",
                            2,
                            WorkHistoryKind::Assistant,
                            "delegated needle",
                        );
                        delegated.thread_id = Some("thread-1".into());
                        delegated
                    },
                ],
            )
            .unwrap();
        let isolated_project = database.open_project(isolated_root.path()).unwrap();
        let isolated = conversation("isolated", Some(isolated_project.id));
        database.save_conversation(&isolated).unwrap();
        database
            .append_history_entries(
                "isolated",
                &[entry(
                    "isolated-entry",
                    1,
                    WorkHistoryKind::User,
                    "delegated needle",
                )],
            )
            .unwrap();

        let tool = HistoryTool::new(database.clone());
        let delegated_context = context("agent-thread:thread-1", root.path());
        let search = tool
            .execute(
                &delegated_context,
                &json!({"operation": "search", "query": "delegated"}),
            )
            .await
            .unwrap();
        assert!(search.content.contains("parent-entry"));
        assert!(search.content.contains("delegated-entry"));

        let all = tool
            .execute(
                &delegated_context,
                &json!({"operation": "search", "query": "needle", "all": true}),
            )
            .await
            .unwrap();
        assert!(!all.content.contains("isolated-entry"));

        let registry = ToolRegistry::new();
        crate::tool::builtin::register_work_tools(&registry, database);
        assert!(registry.get("history").is_some());
        assert!(registry.fork().get("history").is_some());
    }

    #[tokio::test]
    async fn search_pagination_reports_total_and_consistent_offsets() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        database
            .append_history_entries(
                "current",
                &[
                    entry("page-1", 1, WorkHistoryKind::User, "page needle one"),
                    entry("page-2", 2, WorkHistoryKind::User, "page needle two"),
                    entry("page-3", 3, WorkHistoryKind::User, "page needle three"),
                ],
            )
            .unwrap();
        let result = HistoryTool::new(database)
            .execute(
                &context("current", root.path()),
                &json!({"operation": "search", "query": "needle", "limit": 1, "offset": 1}),
            )
            .await
            .unwrap();
        let metadata = result.metadata.as_ref().unwrap();
        assert_eq!(metadata["total"], 3);
        assert_eq!(metadata["offset"], 1);
        assert_eq!(metadata["result_count"], 1);
        assert_eq!(metadata["has_more"], true);
        assert_eq!(metadata["next_offset"], 2);
        assert!(result.content.contains("page-2"));
    }

    #[tokio::test]
    async fn read_rejects_offsets_beyond_unicode_total() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        database
            .append_history_entries(
                "current",
                &[entry("short", 1, WorkHistoryKind::Assistant, "é🙂")],
            )
            .unwrap();
        let error = HistoryTool::new(database)
            .execute(
                &context("current", root.path()),
                &json!({"operation": "read", "id": "short", "offset": 3}),
            )
            .await
            .unwrap_err();
        match error {
            ToolError::InvalidParams { message, .. } => {
                assert!(message.contains("offset"));
                assert!(message.contains("2"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_does_not_discard_originals_after_candidate_boundary() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        let mut entries = (0..512)
            .map(|sequence| {
                entry(
                    &format!("context-{sequence}"),
                    sequence,
                    WorkHistoryKind::ContextWindow,
                    "boundary needle",
                )
            })
            .collect::<Vec<_>>();
        entries.push(entry(
            "original",
            1_000,
            WorkHistoryKind::User,
            "boundary needle original",
        ));
        database
            .append_history_entries("current", &entries)
            .unwrap();
        let result = HistoryTool::new(database)
            .execute(
                &context("current", root.path()),
                &json!({"operation": "search", "query": "boundary", "limit": 1}),
            )
            .await
            .unwrap();
        assert!(result.content.contains("original"));
    }

    #[tokio::test]
    async fn read_returns_unicode_safe_pages_and_metadata() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        let text = "é🙂漢字".repeat(2_000);
        database
            .append_history_entries(
                "current",
                &[entry("long-entry", 1, WorkHistoryKind::Assistant, &text)],
            )
            .unwrap();
        let tool = HistoryTool::new(database);
        let ctx = context("current", root.path());
        let first = tool
            .execute(&ctx, &json!({"operation": "read", "id": "long-entry"}))
            .await
            .unwrap();
        let metadata = first.metadata.as_ref().unwrap();
        assert_eq!(metadata["start"], 0);
        assert_eq!(metadata["total"], text.chars().count());
        assert_eq!(metadata["has_more"], true);
        let next_offset = metadata["next_offset"].as_u64().unwrap() as usize;
        let second = tool
            .execute(
                &ctx,
                &json!({"operation": "read", "id": "long-entry", "offset": next_offset}),
            )
            .await
            .unwrap();
        assert_eq!(second.metadata.as_ref().unwrap()["start"], next_offset);
        assert_eq!(
            format!("{}{}", first.content, second.content)
                .chars()
                .count(),
            text.chars().count()
        );
    }

    #[tokio::test]
    async fn read_reports_missing_ids_clearly() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        let error = HistoryTool::new(database)
            .execute(
                &context("current", root.path()),
                &json!({"operation": "read", "id": "missing"}),
            )
            .await
            .unwrap_err();
        match error {
            ToolError::InvalidParams { message, .. } => assert!(message.contains("missing")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_summarizes_images_but_read_returns_them() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        let mut image_entry = entry(
            "image-entry",
            1,
            WorkHistoryKind::ToolResult,
            "image result",
        );
        image_entry.images = vec![ImageSource {
            media_type: "image/png".into(),
            data: "aW1hZ2U=".into(),
        }];
        database
            .append_history_entries("current", &[image_entry])
            .unwrap();
        let tool = HistoryTool::new(database);
        let ctx = context("current", root.path());
        let search = tool
            .execute(&ctx, &json!({"operation": "search", "query": "image"}))
            .await
            .unwrap();
        assert!(search.content.contains("image/png"));
        assert!(!search.content.contains("aW1hZ2U="));

        let read = tool
            .execute(&ctx, &json!({"operation": "read", "id": "image-entry"}))
            .await
            .unwrap();
        assert_eq!(
            read.images,
            vec![ImageSource {
                media_type: "image/png".into(),
                data: "aW1hZ2U=".into(),
            }]
        );
    }

    #[tokio::test]
    async fn history_rejects_unknown_irrelevant_and_explicit_null_fields() {
        let root = tempfile::tempdir().unwrap();
        let current = conversation("current", None);
        let (_directory, database) = database_with_workspace(root.path(), &[current]);
        let tool = HistoryTool::new(database);
        let ctx = context("current", root.path());
        for params in [
            json!({"operation": "search", "query": "x", "unexpected": true}),
            json!({"operation": "search", "query": "x", "limit": null}),
            json!({"operation": "search", "query": "x", "id": "irrelevant"}),
            json!({"operation": "read", "id": "x", "query": "irrelevant"}),
            json!({"operation": "read", "id": "x", "offset": null}),
        ] {
            let error = tool.execute(&ctx, &params).await.unwrap_err();
            assert!(matches!(error, ToolError::InvalidParams { .. }), "{params}");
        }
    }

    #[test]
    fn parameters_describe_strict_search_and_read_operations() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("history.db")).unwrap();
        let schema = HistoryTool::new(database).parameters();
        assert_eq!(schema["type"], Value::String("object".into()));
        assert!(schema["oneOf"].is_array());
        assert!(schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .all(|operation| operation["additionalProperties"] == false));
    }
}
