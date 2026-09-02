use crate::agent::orchestration::AgentCallRequest;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct CallAgentsTool;

#[derive(Debug, Deserialize)]
struct CallAgentParams {
    prompt: String,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
}

#[async_trait]
impl Tool for CallAgentsTool {
    fn name(&self) -> &str {
        "call_agents"
    }

    fn description(&self) -> &str {
        "Lists available agents and then runs one in a bilateral thread. The delegated agent inherits this conversation's objective and tools."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The focused task for the delegated agent."
                },
                "model_id": {
                    "type": "string",
                    "description": "Optional model id. Use default to inherit the conversation model."
                }
                ,"agent_id": {
                    "type": "string",
                    "description": "Agent id returned by list_agents. Use default when no configured agent is needed."
                },
                "thread_id": {
                    "type": "string",
                    "description": "Optional existing delegated thread id to continue the same agent conversation."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params: CallAgentParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let prompt = params.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "prompt cannot be empty".into(),
            });
        }
        let runner = ctx
            .agent_runner
            .as_ref()
            .ok_or_else(|| ToolError::Execution {
                tool: self.name().into(),
                message: "delegated agents are not configured for this agent".into(),
            })?;
        // Enforce the discovery contract in code as well as in the system
        // prompt. A model cannot accidentally invoke an unknown configured
        // agent or skip the catalogue lookup.
        let agents = runner
            .list_agents(&ctx.session_id)
            .await
            .map_err(|message| ToolError::Execution {
                tool: self.name().into(),
                message,
            })?;
        let agent_id = params
            .agent_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| "default".into());
        if !agents.iter().any(|agent| agent.id == agent_id) {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!(
                    "unknown agent '{}'; call list_agents and choose one of: {}",
                    agent_id,
                    agents
                        .iter()
                        .map(|agent| agent.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        let thread_id = params
            .thread_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let request = AgentCallRequest {
            parent_session_id: ctx.session_id.clone(),
            parent_agent_id: ctx.agent_id.clone(),
            thread_id,
            parent_objective: parent_objective(&ctx.conversation_context, prompt),
            agent_id,
            tools: ctx
                .enabled_tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
            prompt: prompt.to_string(),
            model_id: params.model_id.filter(|model| !model.trim().is_empty()),
            working_dir: ctx.current_dir(),
            context: reduced_context(&ctx.conversation_context),
        };
        let mut snapshot = if let Some(events) = ctx.agent_event_sink.clone() {
            runner.call_agent_streaming(request, events).await
        } else {
            runner.call_agent(request).await
        }
        .map_err(|message| ToolError::Execution {
            tool: self.name().into(),
            message,
        })?;
        let output = if snapshot.output.is_empty() {
            format!(
                "Delegated agent '{}' finished with no output.",
                snapshot.title
            )
        } else {
            snapshot.output.clone()
        };
        // Metadata is intentionally not relied on for the model-facing
        // result: providers only receive ToolResult.content. Returning the
        // id here makes the bilateral thread address visible to both the
        // user and the parent agent for follow-up calls.
        if snapshot.thread_id.trim().is_empty() {
            snapshot.thread_id = snapshot.id.clone();
        }
        let content = format!(
            "thread_id: {}\nagent_id: {}\nstatus: {:?}\n\n{}",
            snapshot.thread_id, snapshot.agent_id, snapshot.status, output
        );
        Ok(ToolResult::ok(content).with_metadata(json!({ "agent_thread": snapshot })))
    }
}

fn reduced_context(messages: &[crate::provider::ChatMessage]) -> Vec<crate::provider::ChatMessage> {
    const MAX_MESSAGES: usize = 8;
    const MAX_CHARS: usize = 16_000;
    let mut selected = messages
        .iter()
        .filter(|message| {
            matches!(
                message.role,
                crate::provider::types::Role::User | crate::provider::types::Role::Assistant
            )
        })
        .rev()
        .take(MAX_MESSAGES)
        .filter_map(|message| {
            let text = match &message.content {
                crate::provider::types::MessageContent::Text(text) => text.clone(),
                crate::provider::types::MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|part| match part {
                        crate::provider::types::ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };

            // A reduced context is deliberately a text-only transcript. Do
            // not forward provider protocol metadata (function calls, tool
            // results, image parts, or stale call ids) into a new child
            // request: providers can reject those messages as invalid when
            // their matching tool turn is not present.
            (!text.trim().is_empty()).then_some(crate::provider::ChatMessage {
                role: message.role.clone(),
                content: crate::provider::types::MessageContent::Text(text),
                tool_call_id: None,
                tool_calls: None,
            })
        })
        .collect::<Vec<_>>();
    selected.reverse();

    // `run_streaming` appends the child objective as a fresh user turn. A
    // parent request that is still in progress commonly ends in a user
    // message, so remove that trailing turn to avoid an invalid consecutive
    // user-message sequence in stricter provider APIs. The objective is also
    // included in the child's delegation system context.
    while matches!(
        selected.last().map(|message| &message.role),
        Some(crate::provider::types::Role::User)
    ) {
        selected.pop();
    }

    while selected.len() > 1
        && serde_json::to_string(&selected)
            .map(|value| value.chars().count() > MAX_CHARS)
            .unwrap_or(false)
    {
        selected.remove(0);
    }
    selected
}

fn parent_objective(messages: &[crate::provider::ChatMessage], fallback: &str) -> String {
    messages
        .iter()
        .find(|message| message.role == crate::provider::types::Role::User)
        .and_then(|message| match &message.content {
            crate::provider::types::MessageContent::Text(text) => Some(text.trim()),
            crate::provider::types::MessageContent::Parts(_) => None,
        })
        .filter(|text| !text.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FunctionCall, MessageContent, Role, ToolCall};

    #[test]
    fn reduced_context_never_leaves_a_dangling_function_call() {
        let context = vec![
            crate::provider::ChatMessage {
                role: Role::User,
                content: MessageContent::Text("parent objective".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            crate::provider::ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(String::new()),
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "call_agents".into(),
                        arguments: "{}".into(),
                    },
                }]),
            },
        ];

        let reduced = reduced_context(&context);
        assert!(reduced.is_empty());
    }

    #[test]
    fn reduced_context_is_text_only_and_does_not_end_with_a_user_turn() {
        let context = vec![
            crate::provider::ChatMessage {
                role: Role::User,
                content: MessageContent::Text("parent objective".into()),
                tool_call_id: None,
                tool_calls: None,
            },
            crate::provider::ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    crate::provider::types::ContentPart::Text {
                        text: "progress".into(),
                    },
                    crate::provider::types::ContentPart::ToolUse {
                        id: "call-1".into(),
                        name: "echo".into(),
                        input: serde_json::json!({"text": "ok"}),
                    },
                ]),
                tool_call_id: Some("stale-call".into()),
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "echo".into(),
                        arguments: r#"{"text":"ok"}"#.into(),
                    },
                }]),
            },
            crate::provider::ChatMessage {
                role: Role::User,
                content: MessageContent::Text("the current request".into()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let reduced = reduced_context(&context);
        assert_eq!(reduced.len(), 2);
        assert_eq!(reduced[1].content, MessageContent::Text("progress".into()));
        assert!(reduced
            .iter()
            .all(|message| message.tool_call_id.is_none() && message.tool_calls.is_none()));
        assert!(!matches!(
            reduced.last().map(|message| &message.role),
            Some(Role::User)
        ));
    }

    #[test]
    fn parent_objective_uses_the_first_user_message() {
        let context = vec![crate::provider::ChatMessage {
            role: Role::User,
            content: MessageContent::Text("original objective".into()),
            tool_call_id: None,
            tool_calls: None,
        }];
        assert_eq!(parent_objective(&context, "fallback"), "original objective");
    }
}
