use super::{Agent, AgentStreamEvent, ContextRequest};
use crate::provider::types::{ContentPart, MessageContent, Role, ToolCall};
use crate::provider::{ChatMessage, ChatResponse};
use crate::storage::work::{WorkHistoryEntry, WorkHistoryKind};
use crate::tool::builtin::ask_user::redact_confirmation_params;
use crate::tool::{EnabledTool, ToolContext, ToolRegistry, ToolResult};
use anyhow::Result;
use futures::future::join_all;
use serde_json::Value;
use std::sync::Arc;

pub(super) struct ToolExecution {
    pub messages: Vec<ChatMessage>,
    pub history_entries: Vec<WorkHistoryEntry>,
    pub context_action: Option<ContextRequest>,
    pub had_failure: bool,
}

struct ToolCallExecution {
    message: ChatMessage,
    history_entry: WorkHistoryEntry,
    context_action: Option<ContextRequest>,
    had_failure: bool,
}

struct PendingContextGuard {
    controller: Arc<super::ContextController>,
    previous: Option<ContextRequest>,
    armed: bool,
}

impl PendingContextGuard {
    fn new(controller: Arc<super::ContextController>) -> Self {
        let previous = controller.pending_request();
        Self {
            controller,
            previous,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }

    fn clear_and_disarm(mut self) {
        self.controller.clear_pending_request();
        self.armed = false;
    }
}

impl Drop for PendingContextGuard {
    fn drop(&mut self) {
        if self.armed {
            self.controller
                .restore_pending_request(self.previous.take());
        }
    }
}

impl Agent {
    pub(super) async fn execute_tools(
        &self,
        response: &ChatResponse,
        events: Option<&tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> Result<ToolExecution> {
        let tool_calls = match &response.message.tool_calls {
            Some(tool_calls) => tool_calls,
            None => {
                return Ok(ToolExecution {
                    messages: Vec::new(),
                    history_entries: Vec::new(),
                    context_action: None,
                    had_failure: false,
                })
            }
        };

        let pending_context_guard = PendingContextGuard::new(self.context_controller.clone());

        let available_tools = self
            .tool_registry
            .catalog()
            .into_iter()
            .filter(|tool| self.config.allow_delegation || !super::is_delegation_tool(&tool.name))
            .collect();
        let ctx = ToolContext {
            working_dir: self.working_dir.clone(),
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
            enabled_tools: self
                .build_tool_definitions()
                .into_iter()
                .map(|definition| EnabledTool {
                    name: definition.name,
                    description: definition.description,
                })
                .collect(),
            available_tools,
            tool_activation: self.tool_activation.clone(),
            workspace_root: self.workspace_root.clone(),
            context_controller: self.context_controller.clone(),
            conversation_context: self.messages.lock().await.clone(),
            agent_runner: self.agent_runner(),
            memory_search_backend: self.memory_search_backend.read().unwrap().clone(),
            agent_event_sink: events.cloned(),
        };
        let parallel_reads = all_calls_run_in_parallel(&self.tool_registry, tool_calls);
        let executions = if parallel_reads {
            tracing::debug!(
                count = tool_calls.len(),
                session_id = %self.session_id,
                "executing independent read-only tools in parallel"
            );
            join_all(
                tool_calls
                    .iter()
                    .map(|tool_call| self.execute_tool_call(tool_call, &ctx, events)),
            )
            .await
        } else {
            let mut executions = Vec::with_capacity(tool_calls.len());
            for tool_call in tool_calls {
                executions.push(self.execute_tool_call(tool_call, &ctx, events).await);
            }
            executions
        };

        let had_failure = executions.iter().any(|execution| execution.had_failure);
        let context_action = if had_failure {
            None
        } else {
            executions
                .iter()
                .find_map(|execution| execution.context_action.clone())
        };
        let history_entries = executions
            .iter()
            .map(|execution| execution.history_entry.clone())
            .collect();
        let messages = executions
            .into_iter()
            .map(|execution| execution.message)
            .collect::<Vec<_>>();

        if had_failure {
            pending_context_guard.clear_and_disarm();
        } else {
            pending_context_guard.disarm();
        }

        Ok(ToolExecution {
            messages,
            history_entries,
            context_action,
            had_failure,
        })
    }

    async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        ctx: &ToolContext,
        events: Option<&tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> ToolCallExecution {
        let tool_name = crate::tool::canonical_tool_name(&tool_call.function.name).to_owned();
        let delegation_blocked =
            !self.config.allow_delegation && super::is_delegation_tool(tool_name.as_str());
        let user_question_blocked = !self.user_questions_allowed() && tool_name == "ask_user";
        if delegation_blocked
            || user_question_blocked
            || !ctx.enabled_tools.iter().any(|tool| tool.name == tool_name)
        {
            let message = if user_question_blocked {
                "Tool 'ask_user' is unavailable in a non-interactive scheduled run.".to_owned()
            } else if delegation_blocked {
                format!(
                    "Tool '{}' is unavailable: delegated agents are leaf workers and cannot launch subagents.",
                    tool_name
                )
            } else {
                format!("Tool '{}' is not available in this request.", tool_name)
            };
            if let Some(events) = events {
                let _ = events.send(AgentStreamEvent::ToolStarted {
                    call_id: Some(tool_call.id.clone()),
                    name: tool_name.clone(),
                    input: serde_json::Value::String(tool_call.function.arguments.clone()),
                });
                let _ = events.send(AgentStreamEvent::ToolFinished {
                    call_id: Some(tool_call.id.clone()),
                    name: tool_name.clone(),
                    success: false,
                    summary: message.clone(),
                    output: message.clone(),
                    metadata: None,
                    images: Vec::new(),
                });
            }
            self.record_tool_outcome(&tool_call.id, false);
            return ToolCallExecution {
                message: ChatMessage {
                    role: Role::Tool,
                    content: MessageContent::Text(message.clone()),
                    tool_call_id: Some(tool_call.id.clone()),
                    tool_calls: None,
                },
                history_entry: self.new_history_entry(
                    WorkHistoryKind::ToolResult,
                    &message,
                    serde_json::json!({
                        "call_id": tool_call.id,
                        "name": tool_name,
                        "success": false,
                    }),
                    Vec::new(),
                ),
                context_action: None,
                had_failure: true,
            };
        }

        let params: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
            Ok(params) => params,
            Err(error) => {
                if let Some(events) = events {
                    let _ = events.send(AgentStreamEvent::ToolStarted {
                        call_id: Some(tool_call.id.clone()),
                        name: tool_name.clone(),
                        input: serde_json::Value::String(tool_call.function.arguments.clone()),
                    });
                    let _ = events.send(AgentStreamEvent::ToolFinished {
                        call_id: Some(tool_call.id.clone()),
                        name: tool_name.clone(),
                        success: false,
                        summary: format!("invalid arguments: {error}"),
                        output: format!("Invalid arguments: {error}"),
                        metadata: None,
                        images: Vec::new(),
                    });
                }
                self.record_tool_outcome(&tool_call.id, false);
                return ToolCallExecution {
                    message: ChatMessage {
                        role: Role::Tool,
                        content: MessageContent::Text(format!("invalid arguments: {error}")),
                        tool_call_id: Some(tool_call.id.clone()),
                        tool_calls: None,
                    },
                    history_entry: self.new_history_entry(
                        WorkHistoryKind::ToolResult,
                        format!("invalid arguments: {error}"),
                        serde_json::json!({
                            "call_id": tool_call.id,
                            "name": tool_name,
                            "success": false,
                        }),
                        Vec::new(),
                    ),
                    context_action: None,
                    had_failure: true,
                };
            }
        };

        if let Some(events) = events {
            let _ = events.send(AgentStreamEvent::ToolStarted {
                call_id: Some(tool_call.id.clone()),
                name: tool_name.clone(),
                input: redact_confirmation_params(&params),
            });
        }

        let result = match self
            .tool_registry
            .execute(tool_name.as_str(), ctx, &params)
            .await
        {
            Ok(result) => result,
            Err(error) => ToolResult::error(error.to_string()),
        };
        self.record_tool_outcome(&tool_call.id, result.success);

        let context_action = if result.success && tool_name == "new_context" {
            result.metadata.as_ref().and_then(|metadata| {
                (metadata.get("context_action").and_then(Value::as_str) == Some("new_context"))
                    .then(|| ContextRequest {
                        handoff: metadata
                            .get("handoff")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
            })
        } else {
            None
        };
        let had_failure = !result.success;

        if matches!(
            tool_name.as_str(),
            "create_global_memory" | "delete_global_memory"
        ) {
            let prompt = result
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("global_memory_prompt"))
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            if let Some(prompt) = prompt {
                self.set_global_memory_prompt(Some(prompt));
            }
        }

        if let Some(events) = events {
            let event_output = if result.success {
                result.content.as_str()
            } else {
                result.error.as_deref().unwrap_or("unknown error")
            };
            let summary = event_output.chars().take(180).collect();
            let output = event_output
                .chars()
                .take(super::MAX_TOOL_EVENT_OUTPUT_CHARS)
                .collect();
            let _ = events.send(AgentStreamEvent::ToolFinished {
                call_id: Some(tool_call.id.clone()),
                name: tool_name.clone(),
                success: result.success,
                summary,
                output,
                metadata: result.metadata.clone(),
                images: result.images.clone(),
            });
        }

        let result_text = if result.success {
            result.content.clone()
        } else {
            result
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".into())
        };
        let history_entry = self.new_history_entry(
            WorkHistoryKind::ToolResult,
            result_text,
            serde_json::json!({
                "call_id": tool_call.id,
                "name": tool_name,
                "arguments": params,
                "success": result.success,
                "metadata": result.metadata,
            }),
            result.images.clone(),
        );

        ToolCallExecution {
            message: ChatMessage {
                role: Role::Tool,
                content: tool_result_message_content(&result),
                tool_call_id: Some(tool_call.id.clone()),
                tool_calls: None,
            },
            history_entry,
            context_action,
            had_failure,
        }
    }
}

fn tool_result_message_content(result: &ToolResult) -> MessageContent {
    let raw_text = if result.success {
        result.content.as_str()
    } else {
        result.error.as_deref().unwrap_or("unknown error")
    };
    let text = crate::compaction::bound_live_tool_output(raw_text);
    if result.images.is_empty() {
        return MessageContent::Text(text);
    }

    let mut parts = Vec::with_capacity(result.images.len() + 1);
    if !text.is_empty() {
        parts.push(ContentPart::Text { text });
    }
    parts.extend(
        result
            .images
            .iter()
            .cloned()
            .map(|source| ContentPart::Image { source }),
    );
    MessageContent::Parts(parts)
}

/// Independent read-only calls can share one round trip without changing the
/// conversation state. Stateful tools, interactive tools, and delegated
/// agents stay sequential so their effects and approval prompts remain
/// deterministic.
fn all_calls_run_in_parallel(tool_registry: &ToolRegistry, tool_calls: &[ToolCall]) -> bool {
    tool_calls.len() > 1
        && tool_calls.iter().all(|tool_call| {
            let name = crate::tool::canonical_tool_name(&tool_call.function.name);
            if name == "ask_user" || super::is_delegation_tool(name) {
                return false;
            }
            tool_registry
                .get(name)
                .is_some_and(|tool| tool.is_read_only() && !tool.requires_confirmation())
        })
}

#[cfg(test)]
mod tests {
    use super::{all_calls_run_in_parallel, tool_result_message_content};
    use crate::provider::types::{ContentPart, FunctionCall, MessageContent, ToolCall};
    use crate::tool::{Tool, ToolRegistry, ToolResult};
    use async_trait::async_trait;
    use serde_json::{json, Value};

    struct ReadOnlyTool;

    #[async_trait]
    impl Tool for ReadOnlyTool {
        fn name(&self) -> &str {
            "read_only"
        }

        fn description(&self) -> &str {
            "Read-only test tool"
        }

        fn parameters(&self) -> Value {
            json!({ "type": "object" })
        }

        async fn execute(
            &self,
            _ctx: &crate::tool::ToolContext,
            _params: &Value,
        ) -> crate::tool::Result<crate::tool::ToolResult> {
            Ok(crate::tool::ToolResult::ok("ok"))
        }

        fn is_read_only(&self) -> bool {
            true
        }
    }

    struct StatefulTool;

    #[async_trait]
    impl Tool for StatefulTool {
        fn name(&self) -> &str {
            "stateful"
        }

        fn description(&self) -> &str {
            "Stateful test tool"
        }

        fn parameters(&self) -> Value {
            json!({ "type": "object" })
        }

        async fn execute(
            &self,
            _ctx: &crate::tool::ToolContext,
            _params: &Value,
        ) -> crate::tool::Result<crate::tool::ToolResult> {
            Ok(crate::tool::ToolResult::ok("ok"))
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: format!("{name}-call"),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    #[test]
    fn independent_read_only_calls_run_in_parallel() {
        let registry = ToolRegistry::new();
        registry.register(ReadOnlyTool);

        assert!(all_calls_run_in_parallel(
            &registry,
            &[call("read_only"), call("read_only")]
        ));
    }

    #[test]
    fn single_calls_and_stateful_calls_stay_sequential() {
        let registry = ToolRegistry::new();
        registry.register(ReadOnlyTool);
        registry.register(StatefulTool);

        assert!(!all_calls_run_in_parallel(&registry, &[call("read_only")]));
        assert!(!all_calls_run_in_parallel(
            &registry,
            &[call("read_only"), call("stateful")]
        ));
        assert!(!all_calls_run_in_parallel(
            &registry,
            &[call("read_only"), call("missing")]
        ));
    }

    #[test]
    fn interactive_and_delegation_calls_never_run_in_parallel() {
        let registry = ToolRegistry::new();
        registry.register(ReadOnlyTool);

        assert!(!all_calls_run_in_parallel(
            &registry,
            &[call("read_only"), call("ask_user")]
        ));
        assert!(!all_calls_run_in_parallel(
            &registry,
            &[call("read_only"), call("call_agents")]
        ));
    }

    #[test]
    fn tool_images_are_preserved_in_provider_content() {
        let result = ToolResult::ok("Screenshot captured").with_image("image/png", "aW1hZ2U=");

        let MessageContent::Parts(parts) = tool_result_message_content(&result) else {
            panic!("expected multimodal tool content");
        };

        assert!(matches!(
            &parts[0],
            ContentPart::Text { text } if text == "Screenshot captured"
        ));
        assert!(matches!(
            &parts[1],
            ContentPart::Image { source }
                if source.media_type == "image/png" && source.data == "aW1hZ2U="
        ));
    }
}
