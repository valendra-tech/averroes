//! Tools for the small, user-confirmed memory injected into every agent.

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::WorkDatabase;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_GLOBAL_MEMORY_CONTENT_CHARS: usize = 2_000;
const MAX_GLOBAL_MEMORY_ID_CHARS: usize = 128;

pub struct CreateGlobalMemoryTool {
    database: Arc<WorkDatabase>,
}

impl CreateGlobalMemoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateGlobalMemoryParams {
    content: String,
}

#[async_trait]
impl Tool for CreateGlobalMemoryTool {
    fn name(&self) -> &str {
        "create_global_memory"
    }

    fn description(&self) -> &str {
        "Save one user-confirmed, long-lived preference, taste, or fact about the user. Always ask first and pass only the approved sentence. Never save task details or secrets."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "maxLength": MAX_GLOBAL_MEMORY_CONTENT_CHARS,
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
        let params: CreateGlobalMemoryParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let content = params.content.trim();
        if content.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "content is required".into(),
            });
        }
        if content.chars().count() > MAX_GLOBAL_MEMORY_CONTENT_CHARS {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!(
                    "content cannot exceed {MAX_GLOBAL_MEMORY_CONTENT_CHARS} characters"
                ),
            });
        }
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteGlobalMemoryParams {
    memory_id: String,
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
                    "maxLength": MAX_GLOBAL_MEMORY_ID_CHARS,
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
        let params: DeleteGlobalMemoryParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let memory_id = params.memory_id.trim();
        if memory_id.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "memory_id is required".into(),
            });
        }
        if memory_id.chars().count() > MAX_GLOBAL_MEMORY_ID_CHARS {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("memory_id cannot exceed {MAX_GLOBAL_MEMORY_ID_CHARS} characters"),
            });
        }
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

    #[tokio::test]
    async fn rejects_invalid_parameters_before_persistence() {
        let (_directory, database) = database();
        let create = CreateGlobalMemoryTool::new(database.clone());
        let create_invalid = [
            json!({"content": ""}),
            json!({
                "content": "x".repeat(MAX_GLOBAL_MEMORY_CONTENT_CHARS + 1)
            }),
            json!({"content": 42}),
            json!({"content": "valid", "unexpected": true}),
        ];
        for params in create_invalid {
            assert!(matches!(
                create.execute(&context(), &params).await,
                Err(ToolError::InvalidParams { .. })
            ));
        }

        let delete = DeleteGlobalMemoryTool::new(database.clone());
        let delete_invalid = [
            json!({"memory_id": ""}),
            json!({
                "memory_id": "x".repeat(MAX_GLOBAL_MEMORY_ID_CHARS + 1)
            }),
            json!({"memory_id": 42}),
            json!({"memory_id": "valid", "unexpected": true}),
        ];
        for params in delete_invalid {
            assert!(matches!(
                delete.execute(&context(), &params).await,
                Err(ToolError::InvalidParams { .. })
            ));
        }
        assert_eq!(database.global_memory_prompt().unwrap(), None);
    }
}
