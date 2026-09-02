use super::{Agent, AgentStreamEvent};
use crate::provider::types::{ContentPart, MessageContent, Role, ToolCall};
use crate::provider::{ChatMessage, ChatResponse};
use crate::tool::{EnabledTool, ToolContext, ToolResult};
use anyhow::Result;
use futures::future::join_all;

pub(super) struct ToolExecution {
    pub messages: Vec<ChatMessage>,
}

struct ToolCallExecution {
    message: ChatMessage,
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
                })
            }
        };

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
            conversation_context: self.messages.lock().await.clone(),
            agent_runner: self.agent_runner(),
            memory_search_backend: self.memory_search_backend.read().unwrap().clone(),
            agent_event_sink: events.cloned(),
        };
        let parallel_web_tools = tool_calls.len() > 1
            && tool_calls
                .iter()
                .all(|tool_call| is_parallel_web_tool(&tool_call.function.name));
        let executions = if parallel_web_tools {
            tracing::debug!(
                count = tool_calls.len(),
                session_id = %self.session_id,
                "executing independent web tools in parallel"
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

        let messages = executions
            .into_iter()
            .map(|execution| execution.message)
            .collect();

        Ok(ToolExecution { messages })
    }

    async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        ctx: &ToolContext,
        events: Option<&tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>>,
    ) -> ToolCallExecution {
        let delegation_blocked =
            !self.config.allow_delegation && super::is_delegation_tool(&tool_call.function.name);
        if delegation_blocked
            || !ctx
                .enabled_tools
                .iter()
                .any(|tool| tool.name == tool_call.function.name)
        {
            let message = if delegation_blocked {
                format!(
                    "Tool '{}' is unavailable: delegated agents are leaf workers and cannot launch subagents.",
                    tool_call.function.name
                )
            } else {
                format!(
                    "Tool '{}' is not enabled. Use discover_tools, then enable_tools, before invoking it.",
                    tool_call.function.name
                )
            };
            if let Some(events) = events {
                let _ = events.send(AgentStreamEvent::ToolStarted {
                    call_id: Some(tool_call.id.clone()),
                    name: tool_call.function.name.clone(),
                    input: serde_json::Value::String(tool_call.function.arguments.clone()),
                });
                let _ = events.send(AgentStreamEvent::ToolFinished {
                    call_id: Some(tool_call.id.clone()),
                    name: tool_call.function.name.clone(),
                    success: false,
                    summary: message.clone(),
                    output: message.clone(),
                    metadata: None,
                });
            }
            return ToolCallExecution {
                message: ChatMessage {
                    role: Role::Tool,
                    content: MessageContent::Text(message),
                    tool_call_id: Some(tool_call.id.clone()),
                    tool_calls: None,
                },
            };
        }

        let params: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
            Ok(params) => params,
            Err(error) => {
                if let Some(events) = events {
                    let _ = events.send(AgentStreamEvent::ToolStarted {
                        call_id: Some(tool_call.id.clone()),
                        name: tool_call.function.name.clone(),
                        input: serde_json::Value::String(tool_call.function.arguments.clone()),
                    });
                    let _ = events.send(AgentStreamEvent::ToolFinished {
                        call_id: Some(tool_call.id.clone()),
                        name: tool_call.function.name.clone(),
                        success: false,
                        summary: format!("invalid arguments: {error}"),
                        output: format!("Invalid arguments: {error}"),
                        metadata: None,
                    });
                }
                return ToolCallExecution {
                    message: ChatMessage {
                        role: Role::Tool,
                        content: MessageContent::Text(format!("invalid arguments: {error}")),
                        tool_call_id: Some(tool_call.id.clone()),
                        tool_calls: None,
                    },
                };
            }
        };

        if let Some(events) = events {
            let _ = events.send(AgentStreamEvent::ToolStarted {
                call_id: Some(tool_call.id.clone()),
                name: tool_call.function.name.clone(),
                input: params.clone(),
            });
        }

        let result = match self
            .tool_registry
            .execute(&tool_call.function.name, ctx, &params)
            .await
        {
            Ok(result) => result,
            Err(error) => ToolResult::error(error.to_string()),
        };

        if matches!(
            tool_call.function.name.as_str(),
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
            let output = event_output.chars().take(12_000).collect();
            let _ = events.send(AgentStreamEvent::ToolFinished {
                call_id: Some(tool_call.id.clone()),
                name: tool_call.function.name.clone(),
                success: result.success,
                summary,
                output,
                metadata: result.metadata.clone(),
            });
        }

        ToolCallExecution {
            message: ChatMessage {
                role: Role::Tool,
                content: tool_result_message_content(&result),
                tool_call_id: Some(tool_call.id.clone()),
                tool_calls: None,
            },
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

fn is_parallel_web_tool(name: &str) -> bool {
    matches!(name, "web_fetch" | "web_search_intrernal")
}

#[cfg(test)]
mod tests {
    use super::{is_parallel_web_tool, tool_result_message_content};
    use crate::provider::types::{ContentPart, MessageContent};
    use crate::tool::ToolResult;

    #[test]
    fn only_stateless_web_reads_run_in_parallel() {
        assert!(is_parallel_web_tool("web_fetch"));
        assert!(is_parallel_web_tool("web_search_intrernal"));
        assert!(!is_parallel_web_tool("browser"));
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
