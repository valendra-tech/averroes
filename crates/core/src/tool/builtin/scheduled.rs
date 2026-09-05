use crate::connection::{ConnectionId, SessionBinding};
use crate::task::scheduled::{ScheduledTask, ScheduledTaskSchedule, ScheduledTaskService};
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub struct ScheduledTaskListTool {
    service: Arc<ScheduledTaskService>,
}

pub struct AddScheduledTaskTool {
    service: Arc<ScheduledTaskService>,
}

pub struct UpdateScheduledTaskTool {
    service: Arc<ScheduledTaskService>,
}

pub struct DeleteScheduledTaskTool {
    service: Arc<ScheduledTaskService>,
}

impl ScheduledTaskListTool {
    pub fn new(service: Arc<ScheduledTaskService>) -> Self {
        Self { service }
    }
}

impl AddScheduledTaskTool {
    pub fn new(service: Arc<ScheduledTaskService>) -> Self {
        Self { service }
    }
}

impl UpdateScheduledTaskTool {
    pub fn new(service: Arc<ScheduledTaskService>) -> Self {
        Self { service }
    }
}

impl DeleteScheduledTaskTool {
    pub fn new(service: Arc<ScheduledTaskService>) -> Self {
        Self { service }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddScheduledTaskParams {
    title: String,
    prompt: String,
    schedule: ScheduledTaskSchedule,
    connection_id: String,
    model_id: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateScheduledTaskParams {
    task_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    schedule: Option<ScheduledTaskSchedule>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    connection_id: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteScheduledTaskParams {
    task_id: String,
}

fn invalid(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParams {
        tool: tool.into(),
        message: message.into(),
    }
}

fn service_error(tool: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution {
        tool: tool.into(),
        message: error.to_string(),
    }
}

#[async_trait]
impl Tool for ScheduledTaskListTool {
    fn name(&self) -> &str {
        "scheduled_task_list"
    }

    fn description(&self) -> &str {
        "Lists recurring tasks configured for the current workspace, including their schedule and last run status."
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
            .service
            .list(Some(&ctx.working_dir))
            .map_err(|error| service_error(self.name(), error))?;
        if tasks.is_empty() {
            return Ok(
                ToolResult::ok("No scheduled tasks exist for this workspace.")
                    .with_metadata(json!({ "tasks": tasks })),
            );
        }
        let content = tasks
            .iter()
            .map(|task| {
                format!(
                    "- {} [{}] — {} ({})",
                    task.id,
                    if task.enabled { "enabled" } else { "disabled" },
                    task.title,
                    task.schedule.summary()
                )
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
impl Tool for AddScheduledTaskTool {
    fn name(&self) -> &str {
        "add_scheduled_task"
    }

    fn description(&self) -> &str {
        "Creates an enabled recurring agent task. It runs non-interactively through macOS launchd, so it requires an explicit connection and model and always allows registered tools without asking for desktop confirmation."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Short name shown in the scheduled task list." },
                "prompt": { "type": "string", "description": "Complete prompt to send on every run." },
                "schedule": {
                    "oneOf": [
                        { "type": "object", "properties": { "kind": { "const": "interval" }, "seconds": { "type": "integer", "minimum": 60 } }, "required": ["kind", "seconds"], "additionalProperties": false },
                        { "type": "object", "properties": { "kind": { "const": "daily" }, "hour": { "type": "integer", "minimum": 0, "maximum": 23 }, "minute": { "type": "integer", "minimum": 0, "maximum": 59 } }, "required": ["kind", "hour", "minute"], "additionalProperties": false },
                        { "type": "object", "properties": { "kind": { "const": "weekly" }, "weekday": { "type": "integer", "minimum": 0, "maximum": 6 }, "hour": { "type": "integer", "minimum": 0, "maximum": 23 }, "minute": { "type": "integer", "minimum": 0, "maximum": 59 } }, "required": ["kind", "weekday", "hour", "minute"], "additionalProperties": false }
                    ]
                },
                "connection_id": { "type": "string", "description": "Exact configured connection id." },
                "model_id": { "type": "string", "description": "Exact model id available on that connection." },
                "reasoning_effort": { "type": "string" }
            },
            "required": ["title", "prompt", "schedule", "connection_id", "model_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: AddScheduledTaskParams = serde_json::from_value(params.clone())
            .map_err(|error| invalid(self.name(), error.to_string()))?;
        let connection_id = params.connection_id.trim();
        let model_id = params.model_id.trim();
        if connection_id.is_empty() || model_id.is_empty() {
            return Err(invalid(
                self.name(),
                "connection_id and model_id cannot be empty",
            ));
        }
        let task = ScheduledTask::new(
            params.title,
            params.prompt,
            ctx.working_dir.clone(),
            None,
            SessionBinding {
                connection_id: Some(ConnectionId(connection_id.to_owned())),
                model_id: Some(model_id.to_owned()),
                reasoning_effort: params.reasoning_effort,
                approval_policy: crate::tool::ToolApprovalPolicy::AllowAll,
                ..Default::default()
            },
            params.schedule,
        );
        let task = self
            .service
            .save(task)
            .map_err(|error| service_error(self.name(), error))?;
        Ok(ToolResult::ok(format!(
            "Scheduled task {} — {} ({})",
            task.id,
            task.title,
            task.schedule.summary()
        ))
        .with_metadata(json!({ "task": task })))
    }
}

#[async_trait]
impl Tool for UpdateScheduledTaskTool {
    fn name(&self) -> &str {
        "update_scheduled_task"
    }

    fn description(&self) -> &str {
        "Updates a recurring task by exact id, including its prompt, schedule, binding, or enabled state."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "title": { "type": "string" },
                "prompt": { "type": "string" },
                "schedule": { "type": "object" },
                "enabled": { "type": "boolean" },
                "connection_id": { "type": "string" },
                "model_id": { "type": "string" },
                "reasoning_effort": { "type": "string" }
            },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: UpdateScheduledTaskParams = serde_json::from_value(params.clone())
            .map_err(|error| invalid(self.name(), error.to_string()))?;
        let id = params.task_id.trim();
        if id.is_empty() {
            return Err(invalid(self.name(), "task_id cannot be empty"));
        }
        let mut task = self
            .service
            .get(id)
            .map_err(|error| service_error(self.name(), error))?;
        if let Some(title) = params.title {
            task.title = title;
        }
        if let Some(prompt) = params.prompt {
            task.prompt = prompt;
        }
        if let Some(schedule) = params.schedule {
            task.schedule = schedule;
        }
        if let Some(enabled) = params.enabled {
            task.enabled = enabled;
        }
        if params.connection_id.is_some() || params.model_id.is_some() {
            task.binding.connection_id = params
                .connection_id
                .map(|value| ConnectionId(value.trim().to_owned()))
                .or(task.binding.connection_id);
            task.binding.model_id = params
                .model_id
                .map(|value| value.trim().to_owned())
                .or(task.binding.model_id);
        }
        if params.reasoning_effort.is_some() {
            task.binding.reasoning_effort = params.reasoning_effort;
        }
        let task = self
            .service
            .save(task)
            .map_err(|error| service_error(self.name(), error))?;
        Ok(
            ToolResult::ok(format!("Updated scheduled task {}", task.id))
                .with_metadata(json!({ "task": task })),
        )
    }
}

#[async_trait]
impl Tool for DeleteScheduledTaskTool {
    fn name(&self) -> &str {
        "delete_scheduled_task"
    }

    fn description(&self) -> &str {
        "Deletes a recurring task by exact id and unloads its macOS LaunchAgent."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" } },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, _: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: DeleteScheduledTaskParams = serde_json::from_value(params.clone())
            .map_err(|error| invalid(self.name(), error.to_string()))?;
        let id = params.task_id.trim();
        if id.is_empty() {
            return Err(invalid(self.name(), "task_id cannot be empty"));
        }
        let deleted = self
            .service
            .delete(id)
            .map_err(|error| service_error(self.name(), error))?;
        if !deleted {
            return Err(invalid(
                self.name(),
                format!("no scheduled task with id '{id}' exists"),
            ));
        }
        Ok(ToolResult::ok(format!("Deleted scheduled task {id}")))
    }
}
