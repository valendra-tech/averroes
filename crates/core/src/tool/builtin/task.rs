use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::{now, TaskPriority, TaskStatus, WorkDatabase, WorkTask};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;
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

pub struct UpdateTaskTool {
    database: Arc<WorkDatabase>,
}

impl UpdateTaskTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddTaskParams {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parent_task_id: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    priority: TaskPriority,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkTaskAsDoneParams {
    task_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateTaskParams {
    task_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<Option<String>>,
    #[serde(default)]
    status: Option<TaskStatus>,
    #[serde(default)]
    priority: Option<TaskPriority>,
    #[serde(default)]
    parent_task_id: Option<Option<String>>,
    #[serde(default)]
    depends_on: Option<Vec<String>>,
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

fn normalize_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn scope_reference(
    ctx: &ToolContext,
    conversation_id: &str,
    value: &str,
    field: &str,
    tool: &str,
) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(tool, format!("{field} cannot be empty")));
    }
    Ok(ctx.tool_activation.work_scope(conversation_id, value).1)
}

fn validate_task_references(
    tasks: &[WorkTask],
    task_id: &str,
    parent_task_id: Option<&str>,
    depends_on: &[String],
    tool: &str,
) -> Result<()> {
    let known = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();

    if let Some(parent_task_id) = parent_task_id {
        if parent_task_id == task_id {
            return Err(invalid(tool, "a task cannot be its own parent"));
        }
        if !known.contains(parent_task_id) {
            return Err(invalid(
                tool,
                format!("parent task '{parent_task_id}' does not exist"),
            ));
        }

        let mut ancestor = Some(parent_task_id);
        let mut visited = BTreeSet::new();
        while let Some(id) = ancestor {
            if id == task_id {
                return Err(invalid(
                    tool,
                    "parent_task_id would create a hierarchy cycle",
                ));
            }
            if !visited.insert(id) {
                return Err(invalid(
                    tool,
                    "parent_task_id would create a hierarchy cycle",
                ));
            }
            ancestor = tasks
                .iter()
                .find(|task| task.id == id)
                .and_then(|task| task.parent_task_id.as_deref());
        }
    }

    let mut dependencies = BTreeSet::new();
    for dependency_id in depends_on {
        if dependency_id == task_id {
            return Err(invalid(tool, "a task cannot depend on itself"));
        }
        if !known.contains(dependency_id.as_str()) {
            return Err(invalid(
                tool,
                format!("dependency task '{dependency_id}' does not exist"),
            ));
        }
        if !dependencies.insert(dependency_id) {
            return Err(invalid(
                tool,
                format!("dependency task '{dependency_id}' is listed more than once"),
            ));
        }
    }
    Ok(())
}

fn ensure_dependencies_complete(tasks: &[WorkTask], task: &WorkTask, tool: &str) -> Result<()> {
    let incomplete = task
        .depends_on
        .iter()
        .filter_map(|dependency_id| {
            tasks
                .iter()
                .find(|candidate| &candidate.id == dependency_id)
        })
        .filter(|dependency| dependency.status != TaskStatus::Done)
        .map(|dependency| dependency.id.as_str())
        .collect::<Vec<_>>();
    if incomplete.is_empty() {
        Ok(())
    } else {
        Err(invalid(
            tool,
            format!(
                "cannot complete '{}' while dependencies are open: {}",
                task.id,
                incomplete.join(", ")
            ),
        ))
    }
}

fn task_marker(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => " ",
        TaskStatus::InProgress => ">",
        TaskStatus::Done => "x",
        TaskStatus::Blocked => "!",
        TaskStatus::Cancelled => "-",
    }
}

fn append_task_tree(
    task: &WorkTask,
    tasks: &[WorkTask],
    depth: usize,
    visited: &mut BTreeSet<String>,
    lines: &mut Vec<String>,
) {
    if !visited.insert(task.id.clone()) {
        return;
    }
    let mut line = format!(
        "{}- [{}] {} ({}, {}) — {}",
        "  ".repeat(depth),
        task_marker(task.status),
        task.id,
        task.priority.as_str(),
        task.status.as_str(),
        task.title
    );
    if let Some(description) = &task.description {
        line.push_str(" — ");
        line.push_str(description);
    }
    if !task.depends_on.is_empty() {
        line.push_str(" [depends on: ");
        line.push_str(&task.depends_on.join(", "));
        line.push(']');
    }
    lines.push(line);

    let child_ids = tasks
        .iter()
        .filter(|candidate| candidate.parent_task_id.as_deref() == Some(task.id.as_str()))
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    for child_id in child_ids {
        if let Some(child) = tasks.iter().find(|candidate| candidate.id == child_id) {
            append_task_tree(child, tasks, depth + 1, visited, lines);
        }
    }
}

fn render_task_tree(tasks: &[WorkTask]) -> String {
    let mut visited = BTreeSet::new();
    let mut lines = Vec::new();
    for task in tasks.iter().filter(|task| task.parent_task_id.is_none()) {
        append_task_tree(task, tasks, 0, &mut visited, &mut lines);
    }
    for task in tasks {
        if !visited.contains(&task.id) {
            append_task_tree(task, tasks, 0, &mut visited, &mut lines);
        }
    }
    lines.join("\n")
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }

    fn description(&self) -> &str {
        "Lists persistent tasks as a hierarchy with stable ids, descriptions, priorities, dependencies, and lifecycle status. Call before updating or completing an unknown task."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, _: &serde_json::Value) -> Result<ToolResult> {
        let (conversation_id, _) = ctx.tool_activation.work_scope(&ctx.session_id, "");
        let tasks =
            self.database
                .tasks(&conversation_id)
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: error.to_string(),
                })?;
        if tasks.is_empty() {
            return Ok(ToolResult::ok("No tasks exist for this conversation yet.")
                .with_metadata(json!({ "tasks": tasks })));
        }
        let content = render_task_tree(&tasks);
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
        "Adds one pending persistent task. For complex work, create a parent plus concrete subtasks using description, priority, parent_task_id, and depends_on. Title is a short remaining action, not narration."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short action that remains to do. Not a plan, status update, or narration."
                },
                "description": {
                    "type": "string",
                    "description": "Concrete scope or acceptance criteria for the task."
                },
                "parent_task_id": {
                    "type": "string",
                    "description": "Exact id of the parent task when this is a subtask."
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact task ids that must be completed before this task."
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high"],
                    "description": "Urgency of the task; defaults to normal."
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
        let random = uuid::Uuid::new_v4().simple().to_string();
        let (conversation_id, task_id) = ctx
            .tool_activation
            .work_scope(&ctx.session_id, &format!("task-{}", &random[..8]));
        let existing_tasks = self
            .database
            .tasks(&conversation_id)
            .map_err(|error| database_error(self.name(), error))?;
        let parent_task_id = params
            .parent_task_id
            .as_deref()
            .map(|parent_task_id| {
                scope_reference(
                    ctx,
                    &conversation_id,
                    parent_task_id,
                    "parent_task_id",
                    self.name(),
                )
            })
            .transpose()?;
        let mut depends_on = Vec::with_capacity(params.depends_on.len());
        for dependency_id in params.depends_on {
            depends_on.push(scope_reference(
                ctx,
                &conversation_id,
                &dependency_id,
                "depends_on task id",
                self.name(),
            )?);
        }
        let (conversation_id, task_id) = ctx.tool_activation.work_scope(&conversation_id, &task_id);
        validate_task_references(
            &existing_tasks,
            &task_id,
            parent_task_id.as_deref(),
            &depends_on,
            self.name(),
        )?;
        let timestamp = now();
        let task = WorkTask {
            id: task_id,
            title: title.to_owned(),
            description: normalize_text(params.description),
            parent_task_id,
            depends_on,
            priority: params.priority,
            status: TaskStatus::Pending,
            created_at: timestamp,
            updated_at: timestamp,
        };
        self.database
            .upsert_task(&conversation_id, &task)
            .map_err(|error| database_error(self.name(), error))?;
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
        "Marks a persistent task done by the exact task_id from task_list. Do not invent ids. Call immediately when the work is finished."
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
        let (conversation_id, task_id) = ctx.tool_activation.work_scope(&ctx.session_id, task_id);
        let tasks = self
            .database
            .tasks(&conversation_id)
            .map_err(|error| database_error(self.name(), error))?;
        let task = tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or_else(|| {
                invalid(
                    self.name(),
                    format!("no task with id '{task_id}' exists in this conversation"),
                )
            })?;
        ensure_dependencies_complete(&tasks, task, self.name())?;
        let task = self
            .database
            .mark_task_as_done(&conversation_id, &task_id, now())
            .map_err(|error| database_error(self.name(), error))?
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

#[async_trait]
impl Tool for UpdateTaskTool {
    fn name(&self) -> &str {
        "update_task"
    }

    fn description(&self) -> &str {
        "Edits a persistent task or changes its status. Use in_progress when starting, blocked when waiting on an external condition, cancelled when intentionally dropped, and done only after verification."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Exact task id returned by task_list."
                },
                "title": {
                    "type": "string",
                    "description": "Replacement short action title."
                },
                "description": {
                    "type": ["string", "null"],
                    "description": "Replacement scope or acceptance criteria; null clears it."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "done", "blocked", "cancelled"]
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high"]
                },
                "parent_task_id": {
                    "type": ["string", "null"],
                    "description": "Replacement parent task id; null makes this a root task."
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Replacement list of exact task ids that must finish first."
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: UpdateTaskParams = serde_json::from_value(params.clone())
            .map_err(|error| invalid(self.name(), error.to_string()))?;
        let requested_id = params.task_id.trim();
        if requested_id.is_empty() {
            return Err(invalid(self.name(), "task_id cannot be empty"));
        }
        if params.title.is_none()
            && params.description.is_none()
            && params.status.is_none()
            && params.priority.is_none()
            && params.parent_task_id.is_none()
            && params.depends_on.is_none()
        {
            return Err(invalid(
                self.name(),
                "provide at least one field to update besides task_id",
            ));
        }

        let (conversation_id, task_id) = ctx
            .tool_activation
            .work_scope(&ctx.session_id, requested_id);
        let tasks = self
            .database
            .tasks(&conversation_id)
            .map_err(|error| database_error(self.name(), error))?;
        let mut task = tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .ok_or_else(|| {
                invalid(
                    self.name(),
                    format!("no task with id '{task_id}' exists in this conversation"),
                )
            })?;

        if let Some(title) = params.title {
            let title = title.trim();
            if title.is_empty() {
                return Err(invalid(self.name(), "title cannot be empty"));
            }
            task.title = title.to_owned();
        }
        if let Some(description) = params.description {
            task.description = normalize_text(description);
        }
        if let Some(status) = params.status {
            task.status = status;
        }
        if let Some(priority) = params.priority {
            task.priority = priority;
        }
        if let Some(parent_task_id) = params.parent_task_id {
            task.parent_task_id = parent_task_id
                .as_deref()
                .map(|parent_task_id| {
                    scope_reference(
                        ctx,
                        &conversation_id,
                        parent_task_id,
                        "parent_task_id",
                        self.name(),
                    )
                })
                .transpose()?;
        }
        if let Some(depends_on) = params.depends_on {
            task.depends_on = depends_on
                .iter()
                .map(|dependency_id| {
                    scope_reference(
                        ctx,
                        &conversation_id,
                        dependency_id,
                        "depends_on task id",
                        self.name(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
        }

        let mut all_tasks = tasks;
        if let Some(existing) = all_tasks
            .iter_mut()
            .find(|candidate| candidate.id == task.id)
        {
            *existing = task.clone();
        }
        validate_task_references(
            &all_tasks,
            &task.id,
            task.parent_task_id.as_deref(),
            &task.depends_on,
            self.name(),
        )?;
        if task.status == TaskStatus::Done {
            ensure_dependencies_complete(&all_tasks, &task, self.name())?;
        }
        task.updated_at = now();
        self.database
            .upsert_task(&conversation_id, &task)
            .map_err(|error| database_error(self.name(), error))?;
        Ok(ToolResult::ok(format!(
            "Updated task {} — {} ({})",
            task.id,
            task.title,
            task.status.as_str()
        ))
        .with_metadata(json!({ "task": task })))
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

    #[tokio::test]
    async fn delegated_task_is_persisted_on_parent_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
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
        let activation = Arc::new(crate::tool::ToolActivation::default());
        activation.set_work_scope("parent-session", "agent:research:");
        let delegated_context = ToolContext {
            session_id: "agent-thread:research".into(),
            tool_activation: activation,
            ..context()
        };

        let task = AddTaskTool::new(database.clone())
            .execute(
                &delegated_context,
                &json!({ "title": "Inspect the repository" }),
            )
            .await
            .unwrap()
            .metadata
            .unwrap()["task"]
            .clone();
        let task_id = task["id"].as_str().unwrap().to_owned();

        UpdateTaskTool::new(database.clone())
            .execute(
                &delegated_context,
                &json!({ "task_id": task_id, "status": "in_progress" }),
            )
            .await
            .unwrap();

        let tasks = database.tasks("parent-session").unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].id.starts_with("agent:research:task-"));
        assert_eq!(tasks[0].status, TaskStatus::InProgress);
        assert!(TaskListTool::new(database)
            .execute(&delegated_context, &json!({}))
            .await
            .unwrap()
            .content
            .contains(&tasks[0].id));
    }

    #[tokio::test]
    async fn complex_tasks_support_descriptions_hierarchy_priorities_and_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        let timestamp = now();
        database
            .save_conversation(&WorkConversation {
                id: "session".into(),
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

        let add = AddTaskTool::new(database.clone());
        let prerequisite = add
            .execute(&context(), &json!({ "title": "Prepare the schema" }))
            .await
            .unwrap()
            .metadata
            .unwrap()["task"]
            .clone();
        let prerequisite_id = prerequisite["id"].as_str().unwrap().to_owned();
        let root = add
            .execute(
                &context(),
                &json!({
                    "title": "Ship the task system",
                    "description": "Deliver the storage, tools, and UI behavior.",
                    "priority": "high"
                }),
            )
            .await
            .unwrap()
            .metadata
            .unwrap()["task"]
            .clone();
        let root_id = root["id"].as_str().unwrap().to_owned();

        let child = add
            .execute(
                &context(),
                &json!({
                    "title": "Implement task persistence",
                    "description": "Store hierarchy and dependencies without losing old tasks.",
                    "parent_task_id": root_id,
                    "depends_on": [prerequisite_id],
                    "priority": "normal"
                }),
            )
            .await
            .unwrap()
            .metadata
            .unwrap()["task"]
            .clone();
        let child_id = child["id"].as_str().unwrap().to_owned();

        let listed = TaskListTool::new(database.clone())
            .execute(&context(), &json!({}))
            .await
            .unwrap();
        assert!(listed.content.contains("Ship the task system"));
        assert!(listed.content.contains("Deliver the storage"));
        assert!(listed.content.contains("Implement task persistence"));
        assert!(listed.content.contains("normal"));

        let blocked = UpdateTaskTool::new(database.clone())
            .execute(
                &context(),
                &json!({ "task_id": child_id, "status": "blocked" }),
            )
            .await
            .unwrap();
        assert!(blocked.content.contains("blocked"));

        let completion_error = UpdateTaskTool::new(database.clone())
            .execute(
                &context(),
                &json!({ "task_id": child_id, "status": "done" }),
            )
            .await
            .unwrap_err();
        assert!(completion_error
            .to_string()
            .contains("dependencies are open"));

        MarkTaskAsDoneTool::new(database.clone())
            .execute(&context(), &json!({ "task_id": prerequisite_id }))
            .await
            .unwrap();
        MarkTaskAsDoneTool::new(database.clone())
            .execute(&context(), &json!({ "task_id": child_id }))
            .await
            .unwrap();

        let updated = UpdateTaskTool::new(database.clone())
            .execute(
                &context(),
                &json!({ "task_id": root_id, "status": "in_progress" }),
            )
            .await
            .unwrap();
        assert!(updated.content.contains("in_progress"));
        assert_eq!(
            database.tasks("session").unwrap()[0].status,
            TaskStatus::InProgress
        );
    }
}
