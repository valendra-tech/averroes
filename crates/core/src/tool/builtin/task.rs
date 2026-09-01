use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::{now, TaskStatus, WorkDatabase, WorkTask};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub struct TaskListTool {
    database: Arc<WorkDatabase>,
}

impl TaskListTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

pub struct AddTaskTool {
    database: Arc<WorkDatabase>,
}

impl AddTaskTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

pub struct MarkTaskAsDoneTool {
    database: Arc<WorkDatabase>,
}

impl MarkTaskAsDoneTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[derive(Deserialize)]
struct AddTaskParams {
    title: String,
}

#[derive(Deserialize)]
struct MarkTaskAsDoneParams {
    task_id: String,
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }

    fn description(&self) -> &str {
        "Lists the persistent tasks for the current conversation, including their stable IDs and completion state."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, _: &serde_json::Value) -> Result<ToolResult> {
        let tasks = self
            .database
            .tasks(&ctx.session_id)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        if tasks.is_empty() {
            return Ok(ToolResult::ok("No tasks exist for this conversation yet.")
                .with_metadata(json!({ "tasks": tasks })));
        }
        let content = tasks
            .iter()
            .map(|task| {
                let marker = match task.status {
                    TaskStatus::Pending => " ",
                    TaskStatus::Done => "x",
                };
                format!("- [{marker}] {} — {}", task.id, task.title)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(content).with_metadata(json!({ "tasks": tasks })))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

#[async_trait]
impl Tool for AddTaskTool {
    fn name(&self) -> &str {
        "add_task"
    }

    fn description(&self) -> &str {
        "Adds one concise, persistent task to the current conversation. Use it for actionable work that should remain visible until completed."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short, action-oriented task title."
                }
            },
            "required": ["title"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: AddTaskParams =
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
        let timestamp = now();
        let random = uuid::Uuid::new_v4().simple().to_string();
        let task = WorkTask {
            id: format!("task-{}", &random[..8]),
            title: title.to_owned(),
            status: TaskStatus::Pending,
            created_at: timestamp,
            updated_at: timestamp,
        };
        self.database
            .upsert_task(&ctx.session_id, &task)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        Ok(
            ToolResult::ok(format!("Added task {} — {}", task.id, task.title))
                .with_metadata(json!({ "task": task })),
        )
    }
}

#[async_trait]
impl Tool for MarkTaskAsDoneTool {
    fn name(&self) -> &str {
        "mark_task_as_done"
    }

    fn description(&self) -> &str {
        "Marks a persistent task as done. Call task_list first when the task ID is unknown."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Exact task ID returned by task_list, for example task-a1b2c3d4."
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: MarkTaskAsDoneParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let task_id = params.task_id.trim();
        if task_id.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "task_id cannot be empty".into(),
            });
        }
        let task = self
            .database
            .mark_task_as_done(&ctx.session_id, task_id, now())
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("no task with id '{task_id}' exists in this conversation"),
            })?;
        Ok(
            ToolResult::ok(format!("Completed task {} — {}", task.id, task.title))
                .with_metadata(json!({ "task": task })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::SessionBinding;
    use crate::work::WorkConversation;
    use serde_json::json;
    use std::path::PathBuf;

    fn context() -> ToolContext {
        ToolContext {
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
        }
    }

    #[tokio::test]
    async fn adds_lists_and_completes_persisted_tasks() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
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

        let add = AddTaskTool::new(database.clone());
        let task = add
            .execute(&context(), &json!({ "title": "Verify the release" }))
            .await
            .unwrap()
            .metadata
            .unwrap()["task"]
            .clone();
        let task_id = task["id"].as_str().unwrap().to_string();

        let listed = TaskListTool::new(database.clone())
            .execute(&context(), &json!({}))
            .await
            .unwrap();
        assert!(listed.content.contains("Verify the release"));

        let completed = MarkTaskAsDoneTool::new(database.clone())
            .execute(&context(), &json!({ "task_id": task_id }))
            .await
            .unwrap();
        assert!(completed.content.contains("Completed task"));
        assert_eq!(
            database.tasks("session").unwrap()[0].status,
            TaskStatus::Done
        );
    }
}
