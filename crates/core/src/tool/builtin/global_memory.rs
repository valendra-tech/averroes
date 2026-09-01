//! Tools for the small, user-confirmed memory injected into every agent.

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::WorkDatabase;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct CreateGlobalMemoryTool {
    database: Arc<WorkDatabase>,
}

impl CreateGlobalMemoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl Tool for CreateGlobalMemoryTool {
    fn name(&self) -> &str {
        "create_global_memory"
    }

    fn description(&self) -> &str {
        "Save one user-confirmed, long-lived preference or fact to global memory. Ask the user for explicit approval before calling this tool."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "A concise, durable fact or preference approved by the user"
                }
            },
            "required": ["content"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let content = params
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "content is required".into(),
            })?;
        let (memory, prompt) = self
            .database
            .create_global_memory(content)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        Ok(ToolResult::ok(format!(
            "Saved confirmed global memory [{}]: {}",
            &memory.id[..8],
            memory.content
        ))
        .with_metadata(json!({
            "global_memory_prompt": prompt,
            "memory_id": memory.id,
            "source": "averroes global memory"
        })))
    }
}

pub struct DeleteGlobalMemoryTool {
    database: Arc<WorkDatabase>,
}

impl DeleteGlobalMemoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl Tool for DeleteGlobalMemoryTool {
    fn name(&self) -> &str {
        "delete_global_memory"
    }

    fn description(&self) -> &str {
        "Delete a user-confirmed global memory by the short ID shown in the global-memory prompt. Ask the user for explicit approval before calling this tool."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "memory_id": {
                    "type": "string",
                    "description": "The full or eight-character global-memory ID to delete"
                }
            },
            "required": ["memory_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let memory_id = params
            .get("memory_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "memory_id is required".into(),
            })?;
        let (deleted, prompt) = self
            .database
            .delete_global_memory(memory_id)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        if !deleted {
            return Ok(ToolResult::error(format!(
                "No global memory matched '{memory_id}'."
            )));
        }
        Ok(
            ToolResult::ok(format!("Deleted global memory [{memory_id}].")).with_metadata(json!({
                "global_memory_prompt": prompt,
                "deleted_memory_id": memory_id,
                "source": "averroes global memory"
            })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn database() -> (tempfile::TempDir, Arc<WorkDatabase>) {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        (directory, database)
    }

    #[tokio::test]
    async fn create_and_delete_regenerate_the_global_prompt() {
        let (_directory, database) = database();
        let create = CreateGlobalMemoryTool::new(database.clone());
        let created = create
            .execute(&context(), &json!({"content":"Prefer English UI copy."}))
            .await
            .unwrap();
        let id = created.metadata.as_ref().unwrap()["memory_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(database
            .global_memory_prompt()
            .unwrap()
            .unwrap()
            .contains("Prefer English UI copy."));

        let delete = DeleteGlobalMemoryTool::new(database.clone());
        delete
            .execute(&context(), &json!({"memory_id": &id[..8]}))
            .await
            .unwrap();
        assert_eq!(database.global_memory_prompt().unwrap(), None);
    }
}
