use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::{now, CheckpointStatus, WorkCheckpoint, WorkDatabase};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub struct CheckpointTool {
    database: Arc<WorkDatabase>,
}

impl CheckpointTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointParams {
    #[serde(default)]
    id: Option<String>,
    title: String,
    status: CheckpointStatus,
    #[serde(default)]
    detail: Option<String>,
}

#[async_trait]
impl Tool for CheckpointTool {
    fn name(&self) -> &str {
        "checkpoint"
    }

    fn description(&self) -> &str {
        "Creates or updates a visible work checkpoint. Multiple checkpoints are allowed per conversation: use a distinct stable id for each milestone and reuse that id to update it. `in_progress` is the loading state; finish it as `completed` or `blocked`. Title is the hover label: a short completed or current outcome, never a plan or narration."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Stable short identifier for this milestone. Reuse it exactly when updating the same checkpoint; use a new id for a different milestone."
                },
                "title": {
                    "type": "string",
                    "description": "Short hover label, at most ~8 words. State the outcome or current milestone, not what you are about to do."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "blocked"]
                },
                "detail": {
                    "type": "string",
                    "description": "Optional. Only a blocker or a concrete result. Never a plan, next step, or tool narration. Omit it when the title is enough."
                }
            },
            "required": ["title", "status"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: CheckpointParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let title = params.title.trim();
        if title.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "title cannot be empty".into(),
            });
        }
        let checkpoint_id = params
            .id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let (conversation_id, checkpoint_id) = ctx
            .tool_activation
            .work_scope(&ctx.session_id, &checkpoint_id);
        let checkpoint = WorkCheckpoint {
            id: checkpoint_id,
            title: title.to_string(),
            status: params.status,
            detail: params.detail.filter(|detail| !detail.trim().is_empty()),
            message_position: None,
            updated_at: now(),
        };
        self.database
            .upsert_checkpoint(&conversation_id, &checkpoint)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        Ok(ToolResult::ok(format!(
            "Checkpoint '{}' is {}",
            checkpoint.title,
            checkpoint.status.as_str()
        ))
        .with_metadata(json!({ "checkpoint": checkpoint })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::SessionBinding;
    use crate::tool::ToolActivation;
    use crate::work::WorkConversation;
    use std::path::PathBuf;

    #[tokio::test]
    async fn creates_and_updates_a_persisted_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("work.db")).unwrap();
        let timestamp = now();
        database
            .save_conversation(&WorkConversation {
                id: "session".into(),
                title: "Test".into(),
                project_id: None,
                pinned: false,
                unread: false,
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
            })
            .unwrap();
        let tool = CheckpointTool::new(database.clone());
        let context = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(crate::tool::ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        };
        tool.execute(
            &context,
            &json!({"id":"ui", "title":"Build UI", "status":"in_progress"}),
        )
        .await
        .unwrap();
        tool.execute(
            &context,
            &json!({"id":"ui", "title":"Build UI", "status":"completed"}),
        )
        .await
        .unwrap();
        let conversation = database.conversation("session").unwrap().unwrap();
        assert_eq!(conversation.checkpoints.len(), 1);
        assert_eq!(
            conversation.checkpoints[0].status,
            CheckpointStatus::Completed
        );
    }

    #[tokio::test]
    async fn delegated_checkpoint_is_persisted_on_parent_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("work.db")).unwrap();
        let timestamp = now();
        database
            .save_conversation(&WorkConversation {
                id: "parent-session".into(),
                title: "Parent".into(),
                project_id: None,
                pinned: false,
                unread: false,
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
            })
            .unwrap();
        let activation = Arc::new(ToolActivation::default());
        activation.set_work_scope("parent-session", "agent:research:");
        let context = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "agent-thread:research".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: activation,
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        };

        CheckpointTool::new(database.clone())
            .execute(
                &context,
                &json!({
                    "id": "research",
                    "title": "Research in progress",
                    "status": "in_progress"
                }),
            )
            .await
            .unwrap();

        let conversation = database.conversation("parent-session").unwrap().unwrap();
        assert_eq!(conversation.checkpoints.len(), 1);
        assert_eq!(conversation.checkpoints[0].id, "agent:research:research");
    }
}
