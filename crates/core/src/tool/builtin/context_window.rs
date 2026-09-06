use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct GetContextRemainingTool;

pub struct NewContextTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetContextRemainingParams {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewContextParams {
    handoff: Option<String>,
}

#[async_trait]
impl Tool for GetContextRemainingTool {
    fn name(&self) -> &str {
        "get_context_remaining"
    }

    fn description(&self) -> &str {
        "Report provider-measured context usage, remaining hard-limit capacity, and automatic rollover status."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let _params: GetContextRemainingParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;

        let budget = ctx.context_controller.budget();
        let usage = ctx.context_controller.usage();
        let Some(input_tokens) = usage.and_then(|usage| usage.input_tokens) else {
            let mut content = format!(
                "Context usage is not known yet because the provider has not reported token usage.\nHard limit: {} tokens total; remaining capacity is unknown.",
                format_tokens(budget.context_window),
            );
            if let Some(status) = automatic_status_line(budget, None) {
                content.push('\n');
                content.push_str(&status);
            }
            return Ok(ToolResult::ok(content));
        };

        let hard_remaining = budget.context_window.saturating_sub(input_tokens);
        let mut content = format!(
            "Context usage: {} input tokens.\nHard limit: {} tokens remaining of {}.",
            format_tokens(input_tokens),
            format_tokens(hard_remaining),
            format_tokens(budget.context_window),
        );

        if let Some(status) = automatic_status_line(budget, Some(input_tokens)) {
            content.push('\n');
            content.push_str(&status);
        }

        Ok(ToolResult::ok(content).with_metadata(json!({
            "input_tokens": input_tokens,
            "context_limit": budget.context_window,
            "hard_remaining_tokens": hard_remaining,
            "automatic_enabled": budget.automatic_enabled(),
            "automatic_remaining_tokens": budget.automatic_enabled().then(|| {
                budget.rollover_at.saturating_sub(input_tokens)
            }),
        })))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

fn automatic_status_line(
    budget: crate::agent::ContextBudget,
    input_tokens: Option<u64>,
) -> Option<String> {
    if budget.automatic_enabled() {
        let input_tokens = input_tokens?;
        let rollover_remaining = budget.rollover_at.saturating_sub(input_tokens);
        Some(format!(
            "Automatic rollover: {} tokens remaining, at {} input tokens.",
            format_tokens(rollover_remaining),
            format_tokens(budget.rollover_at),
        ))
    } else {
        Some(format!(
            "Automatic behavior is off; automatic rollover is disabled because {}.",
            automatic_off_reason(budget),
        ))
    }
}

fn automatic_off_reason(budget: crate::agent::ContextBudget) -> &'static str {
    if !budget.enabled {
        "the context budget is disabled"
    } else if !budget.supported {
        "the context budget is unsupported for this window size"
    } else {
        "the context budget has no usable capacity"
    }
}

fn format_tokens(tokens: u64) -> String {
    let digits = tokens.to_string();
    let first_group_len = digits.len() % 3;
    let first_group_len = if first_group_len == 0 {
        3
    } else {
        first_group_len
    };
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    formatted.push_str(&digits[..first_group_len]);
    for chunk in digits[first_group_len..].as_bytes().chunks(3) {
        formatted.push(',');
        formatted.push_str(std::str::from_utf8(chunk).expect("digits are valid UTF-8"));
    }
    formatted
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
    use super::{GetContextRemainingTool, NewContextTool};
    use crate::agent::{ContextBudget, ContextController, ContextUsage};
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

    #[tokio::test]
    async fn context_remaining_reports_known_usage_and_both_limits() {
        let controller = Arc::new(ContextController::for_test(100_000, 16_384));
        controller.record_usage(ContextUsage::from_usage(76_000, 500, 100_000));
        let result = GetContextRemainingTool
            .execute(&test_context(controller), &json!({}))
            .await
            .unwrap();

        assert!(result.content.contains("7,617"));
        assert!(result.content.contains("24,000"));
    }

    #[tokio::test]
    async fn context_remaining_reports_unknown_usage_before_first_provider_report() {
        let controller = Arc::new(ContextController::for_test(100_000, 16_384));
        let result = GetContextRemainingTool
            .execute(&test_context(controller), &json!({}))
            .await
            .unwrap();

        assert!(result.content.contains("usage is not known"));
    }

    #[tokio::test]
    async fn context_remaining_reports_unknown_usage_and_disabled_reason() {
        let controller = Arc::new(ContextController::new(ContextBudget::new(
            100_000, 16_384, false,
        )));
        let result = GetContextRemainingTool
            .execute(&test_context(controller), &json!({}))
            .await
            .unwrap();

        assert!(result.content.contains("usage is not known"));
        assert!(result.content.contains("Automatic behavior is off"));
        assert!(result.content.contains("the context budget is disabled"));
    }

    #[tokio::test]
    async fn context_remaining_reports_unknown_usage_and_unsupported_reason() {
        let controller = Arc::new(ContextController::new(ContextBudget::new(
            32_000, 24_000, true,
        )));
        let result = GetContextRemainingTool
            .execute(&test_context(controller), &json!({}))
            .await
            .unwrap();

        assert!(result.content.contains("usage is not known"));
        assert!(result.content.contains("Automatic behavior is off"));
        assert!(result
            .content
            .contains("the context budget is unsupported for this window size"));
    }

    #[tokio::test]
    async fn context_remaining_reports_hard_limit_when_budget_is_disabled() {
        let controller = Arc::new(ContextController::new(ContextBudget::new(
            100_000, 16_384, false,
        )));
        controller.record_usage(ContextUsage::from_usage(76_000, 500, 100_000));
        let result = GetContextRemainingTool
            .execute(&test_context(controller), &json!({}))
            .await
            .unwrap();

        assert!(result.content.contains("24,000"));
        assert!(result.content.contains("Automatic behavior is off"));
        assert!(!result.content.contains("Automatic rollover:"));
    }

    #[tokio::test]
    async fn context_remaining_reports_hard_limit_when_budget_is_unsupported() {
        let controller = Arc::new(ContextController::new(ContextBudget::new(
            32_000, 24_000, true,
        )));
        controller.record_usage(ContextUsage::from_usage(5_000, 500, 32_000));
        let result = GetContextRemainingTool
            .execute(&test_context(controller), &json!({}))
            .await
            .unwrap();

        assert!(result.content.contains("27,000"));
        assert!(result.content.contains("Automatic behavior is off"));
        assert!(!result.content.contains("Automatic rollover:"));
    }
}
