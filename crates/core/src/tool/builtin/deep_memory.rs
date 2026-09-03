//! On-demand access to the slower, indexed global conversation archive.

use crate::tool::{MemorySearchBackend, Result, Tool, ToolContext, ToolError, ToolResult};
use crate::work::{DeepMemoryExcerpt, WorkDatabase, WorkMessageRole};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct SearchDeepMemoryTool {
    database: Arc<WorkDatabase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchDeepMemoryParams {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetDeepMemoryParams {
    conversation_id: String,
    start: Option<usize>,
    limit: Option<usize>,
}

impl SearchDeepMemoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl Tool for SearchDeepMemoryTool {
    fn name(&self) -> &str {
        "search_deep_memory"
    }

    fn description(&self) -> &str {
        "Search the slow, indexed archive of past conversations. Use only when an older decision, fact, or prior work is genuinely needed; call get_deep_memory to read the matching conversation."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "A focused search for older work or decisions"},
                "limit": {"type": "integer", "description": "Maximum conversations to return"}
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: SearchDeepMemoryParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let query = required_string(self.name(), &params.query, "query")?;
        let limit = params.limit.unwrap_or(6);
        if !(1..=12).contains(&limit) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "limit must be between 1 and 12".into(),
            });
        }
        let results = search(
            &self.database,
            ctx.memory_search_backend.as_ref(),
            query,
            limit,
        )
        .await?;
        if results.is_empty() {
            return Ok(ToolResult::ok("No relevant deep memory found."));
        }
        let content = results
            .iter()
            .map(|result| {
                format!(
                    "- **{}** (conversation_id: `{}`)\n  {}",
                    result.title, result.conversation_id, result.snippet
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(content).with_metadata(json!({
            "source": "averroes deep conversation memory",
            "result_count": results.len()
        })))
    }
}

pub struct GetDeepMemoryTool {
    database: Arc<WorkDatabase>,
}

impl GetDeepMemoryTool {
    pub fn new(database: Arc<WorkDatabase>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl Tool for GetDeepMemoryTool {
    fn name(&self) -> &str {
        "get_deep_memory"
    }

    fn description(&self) -> &str {
        "Read a bounded, user-visible slice of a past conversation returned by search_deep_memory."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "conversation_id": {"type": "string", "description": "ID returned by search_deep_memory"},
                "start": {"type": "integer", "description": "Zero-based message offset"},
                "limit": {"type": "integer", "description": "Maximum messages to read"}
            },
            "required": ["conversation_id"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: GetDeepMemoryParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let conversation_id =
            required_string(self.name(), &params.conversation_id, "conversation_id")?;
        let start = params.start.unwrap_or(0);
        let limit = params.limit.unwrap_or(8);
        if !(1..=20).contains(&limit) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "limit must be between 1 and 20".into(),
            });
        }
        let excerpt = self
            .database
            .deep_memory_excerpt(conversation_id, start, limit)
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error.to_string(),
            })?
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("conversation '{conversation_id}' was not found"),
            })?;
        Ok(
            ToolResult::ok(format_excerpt(&excerpt)).with_metadata(json!({
                "source": "averroes deep conversation memory",
                "conversation_id": excerpt.conversation_id,
                "message_count": excerpt.messages.len()
            })),
        )
    }
}

async fn search(
    database: &WorkDatabase,
    backend: Option<&Arc<dyn MemorySearchBackend>>,
    query: &str,
    limit: usize,
) -> Result<Vec<crate::work::ConversationSearchResult>> {
    match backend {
        Some(backend) => backend
            .search(query, limit)
            .await
            .map_err(|error| ToolError::Execution {
                tool: "search_deep_memory".into(),
                message: error,
            }),
        None => database
            .search_conversations_text(query, limit)
            .map_err(|error| ToolError::Execution {
                tool: "search_deep_memory".into(),
                message: error.to_string(),
            }),
    }
}

fn required_string<'a>(tool: &str, value: &'a str, key: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: format!("{key} is required"),
        });
    }
    Ok(value)
}

fn format_excerpt(excerpt: &DeepMemoryExcerpt) -> String {
    let mut content = format!(
        "# {}\nConversation ID: {}\n",
        excerpt.title, excerpt.conversation_id
    );
    if let Some(context) = excerpt
        .context_summary
        .as_deref()
        .map(str::trim)
        .filter(|context| !context.is_empty())
    {
        content.push_str("\n[Understood context]\n");
        content.push_str(context);
        content.push('\n');
    }
    for message in &excerpt.messages {
        let role = match message.role {
            WorkMessageRole::User => "User",
            WorkMessageRole::Assistant => "Assistant",
            WorkMessageRole::Error => "Error",
        };
        content.push_str(&format!(
            "\n[{}] {role}: {}\n",
            message.position, message.text
        ));
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::SessionBinding;
    use crate::work::{WorkConversation, WorkMessage};
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
    async fn get_reads_only_the_requested_user_visible_messages() {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        database
            .save_conversation(&WorkConversation {
                id: "previous-work".into(),
                title: "Previous work".into(),
                project_id: None,
                pinned: false,
                unread: false,
                created_at: 1,
                updated_at: 1,
                binding: SessionBinding::default(),
                context_summary: None,
                context_usage: crate::agent::ContextUsage::default(),
                messages: vec![WorkMessage {
                    role: WorkMessageRole::Assistant,
                    text: "The chosen database is SQLite.".into(),
                    reasoning: "never expose this".into(),
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    tool_activities: Vec::new(),
                    expanded_tool_groups: Vec::new(),
                }],
                checkpoints: Vec::new(),
                tasks: Vec::new(),
                sources: Vec::new(),
                agent_threads: Vec::new(),
                agent_thread_transcripts: std::collections::HashMap::new(),
            })
            .unwrap();
        let tool = GetDeepMemoryTool::new(database);

        let result = tool
            .execute(&context(), &json!({"conversation_id":"previous-work"}))
            .await
            .unwrap();
        assert!(result.content.contains("The chosen database is SQLite."));
        assert!(!result.content.contains("never expose this"));
    }
}
