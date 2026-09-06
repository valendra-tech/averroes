use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct NewContextTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewContextParams {
    handoff: Option<String>,
}

#[async_trait]
impl Tool for NewContextTool {
    fn name(&self) -> &str {
        "new_context"
    }

    fn description(&self) -> &str {
        "Start a fresh context window after this tool batch succeeds. Pass concise continuation state in handoff when useful."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handoff": {
                    "type": "string",
                    "description": "Optional concise continuation state for the fresh context window."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: NewContextParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;

        if ctx.context_controller.pending_request().is_some() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "a context action is already pending".into(),
            });
        }

        ctx.context_controller
            .request_context(params.handoff)
            .map_err(|message| ToolError::InvalidParams {
                tool: self.name().into(),
                message,
            })?;

        let handoff = ctx
            .context_controller
            .pending_request()
            .and_then(|request| request.handoff);
        Ok(
            ToolResult::ok("A new context window is pending.").with_metadata(json!({
                "context_action": "new_context",
                "handoff": handoff,
            })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::NewContextTool;
    use crate::agent::ContextController;
    use crate::provider::ChatMessage;
    use crate::tool::{Tool, ToolContext};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_context(controller: Arc<ContextController>) -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            workspace_root: PathBuf::from("/tmp"),
            session_id: "test-session".into(),
            agent_id: "test-agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(crate::tool::ToolActivation::default()),
            context_controller: controller,
            conversation_context: vec![ChatMessage {
                role: crate::provider::types::Role::User,
                content: crate::provider::types::MessageContent::Text("active".into()),
                tool_call_id: None,
                tool_calls: None,
            }],
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[tokio::test]
    async fn new_context_returns_a_pending_action_without_resetting_messages() {
        let controller = Arc::new(ContextController::for_test(100_000, 16_384));
        let ctx = test_context(controller.clone());
        let message_count = ctx.conversation_context.len();

        let result = NewContextTool
            .execute(&ctx, &json!({"handoff":"continue the fix"}))
            .await
            .unwrap();

        assert!(result.success);
        let metadata = result.metadata.as_ref().unwrap();
        assert_eq!(metadata["context_action"], "new_context");
        assert_eq!(metadata["handoff"], "continue the fix");
        assert_eq!(
            controller.pending_request().unwrap().handoff.as_deref(),
            Some("continue the fix")
        );
        assert_eq!(ctx.conversation_context.len(), message_count);
    }

    #[tokio::test]
    async fn new_context_rejects_an_oversized_handoff() {
        let controller = Arc::new(ContextController::for_test(100_000, 16_384));
        let ctx = test_context(controller.clone());
        let handoff = "x".repeat(controller.handoff_limit() + 1);

        let error = NewContextTool
            .execute(&ctx, &json!({"handoff": handoff}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("handoff exceeds"));
        assert!(controller.pending_request().is_none());
    }

    #[tokio::test]
    async fn new_context_rejects_a_second_pending_action() {
        let controller = Arc::new(ContextController::for_test(100_000, 16_384));
        let ctx = test_context(controller.clone());

        NewContextTool
            .execute(&ctx, &json!({"handoff":"first"}))
            .await
            .unwrap();
        let error = NewContextTool
            .execute(&ctx, &json!({"handoff":"second"}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("already pending"));
        assert_eq!(
            controller.pending_request().unwrap().handoff.as_deref(),
            Some("first")
        );
    }
}
