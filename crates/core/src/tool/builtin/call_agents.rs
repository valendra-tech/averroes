use crate::agent::orchestration::AgentCallRequest;
use crate::provider::types::MessageContent;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

const MAX_DELEGATED_PROMPT_CHARS: usize = 32_000;
const MAX_DELEGATED_ID_CHARS: usize = 128;

pub struct CallAgentsTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
        "Runs a selected delegated agent in a bilateral thread. Use list_agents only when you need to choose a specialist; the delegated agent inherits this conversation's objective and tools."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "maxLength": MAX_DELEGATED_PROMPT_CHARS,
                    "description": "The focused task for the delegated agent."
                },
                "model_id": {
                    "type": "string",
                    "maxLength": MAX_DELEGATED_ID_CHARS,
                    "description": "Optional model id. Use default to inherit the conversation model."
                }
                ,"agent_id": {
                    "type": "string",
                    "maxLength": MAX_DELEGATED_ID_CHARS,
                    "description": "Agent id returned by list_agents. Use default when no configured agent is needed."
                },
                "thread_id": {
                    "type": "string",
                    "maxLength": MAX_DELEGATED_ID_CHARS,
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
        let prompt = params.prompt.trim().to_owned();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "prompt cannot be empty".into(),
            });
        }
        if prompt.chars().count() > MAX_DELEGATED_PROMPT_CHARS {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: format!("prompt cannot exceed {MAX_DELEGATED_PROMPT_CHARS} characters"),
            });
        }
        let runner = ctx
            .agent_runner
            .as_ref()
            .ok_or_else(|| ToolError::Execution {
                tool: self.name().into(),
                message: "delegated agents are not configured for this agent".into(),
            })?;
        // Validate the configured agent before starting a bilateral thread.
        let agents = runner
            .list_agents(&ctx.session_id)
            .await
            .map_err(|message| ToolError::Execution {
                tool: self.name().into(),
                message,
            })?;
        let agent_id = params
            .agent_id
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "default".into());
        validate_delegated_id(self.name(), "agent_id", &agent_id)?;
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
        let agent_name = agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| agent_id.clone());
        let thread_id = params
            .thread_id
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_delegated_id(self.name(), "thread_id", &thread_id)?;
        let model_id = params
            .model_id
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty());
        if let Some(model_id) = model_id.as_deref() {
            validate_delegated_id(self.name(), "model_id", model_id)?;
        }
        let request = AgentCallRequest {
            parent_session_id: ctx.session_id.clone(),
            parent_agent_id: ctx.agent_id.clone(),
            thread_id,
            parent_objective: parent_objective(&ctx.conversation_context, &prompt),
            agent_id,
            tools: ctx
                .enabled_tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
            prompt,
            model_id,
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
            "agent_name: {agent_name}\nthread_id: {}\nagent_id: {}\nstatus: {:?}\n\n{}",
            snapshot.thread_id, snapshot.agent_id, snapshot.status, output
        );
        Ok(ToolResult::ok(content).with_metadata(json!({
            "agent_name": agent_name,
            "agent_thread": snapshot
        })))
    }
}

fn validate_delegated_id(tool: &str, field: &str, value: &str) -> Result<()> {
    if value.chars().count() > MAX_DELEGATED_ID_CHARS {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: format!("{field} cannot exceed {MAX_DELEGATED_ID_CHARS} characters"),
        });
    }
    Ok(())
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

    while serde_json::to_string(&selected)
        .map(|value| value.chars().count() > MAX_CHARS)
        .unwrap_or(false)
    {
        if selected.len() > 1 {
            selected.remove(0);
            continue;
        }
        let target = MAX_CHARS.saturating_sub(256);
        let Some(MessageContent::Text(text)) =
            selected.first_mut().map(|message| &mut message.content)
        else {
            break;
        };
        let mut bounded = text.chars().take(target).collect::<String>();
        bounded.push_str("\n[delegated context truncated]");
        *text = bounded;
        while serde_json::to_string(&selected)
            .map(|value| value.chars().count() > MAX_CHARS)
            .unwrap_or(false)
        {
            let Some(MessageContent::Text(text)) =
                selected.first_mut().map(|message| &mut message.content)
            else {
                break;
            };
            if text.pop().is_none() {
                break;
            }
        }
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
    use crate::agent::orchestration::{
        AgentCallRequest, AgentDescriptor, AgentRunner, AgentThreadSnapshot, AgentThreadStatus,
    };
    use crate::provider::types::{FunctionCall, MessageContent, Role, ToolCall};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct CapturingRunner {
        request: Arc<Mutex<Option<AgentCallRequest>>>,
    }

    #[async_trait]
    impl AgentRunner for CapturingRunner {
        async fn list_agents(
            &self,
            _parent_session_id: &str,
        ) -> std::result::Result<Vec<AgentDescriptor>, String> {
            Ok(vec![AgentDescriptor::default()])
        }

        async fn call_agent(
            &self,
            request: AgentCallRequest,
        ) -> std::result::Result<AgentThreadSnapshot, String> {
            *self.request.lock().unwrap() = Some(request.clone());
            Ok(AgentThreadSnapshot {
                id: request.thread_id.clone(),
                thread_id: request.thread_id,
                agent_id: request.agent_id,
                parent_session_id: request.parent_session_id,
                title: "Captured".into(),
                model_id: request.model_id.unwrap_or_else(|| "default".into()),
                status: AgentThreadStatus::Completed,
                enabled_tools: request.tools,
                prompt: request.prompt,
                output: "done".into(),
                created_at: 0,
                updated_at: 0,
            })
        }
    }

    fn context(root: &Path, runner: Arc<CapturingRunner>) -> ToolContext {
        ToolContext {
            working_dir: root.to_path_buf(),
            session_id: "parent-session".into(),
            agent_id: "parent-agent".into(),
            enabled_tools: vec![
                crate::tool::EnabledTool {
                    name: "file_read".into(),
                    description: "Read files".into(),
                },
                crate::tool::EnabledTool {
                    name: "web_fetch".into(),
                    description: "Fetch pages".into(),
                },
            ],
            available_tools: Vec::new(),
            tool_activation: Arc::new(crate::tool::ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: Some(runner),
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }

    #[tokio::test]
    async fn forwards_normalized_request_context_to_the_runner() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let request = Arc::new(Mutex::new(None));
        let runner = Arc::new(CapturingRunner {
            request: request.clone(),
        });
        let context = context(directory.path(), runner);
        context.set_current_dir(nested.clone());

        let result = CallAgentsTool
            .execute(
                &context,
                &serde_json::json!({
                    "prompt": "  inspect the current project  ",
                    "agent_id": "  default  ",
                    "model_id": "  model-x  ",
                    "thread_id": "  thread-x  "
                }),
            )
            .await
            .unwrap();
        let request = request.lock().unwrap().take().unwrap();

        assert!(result.success);
        assert_eq!(request.working_dir, nested);
        assert_eq!(request.tools, vec!["file_read", "web_fetch"]);
        assert_eq!(request.prompt, "inspect the current project");
        assert_eq!(request.agent_id, "default");
        assert_eq!(request.model_id.as_deref(), Some("model-x"));
        assert_eq!(request.thread_id, "thread-x");
    }

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
    fn reduced_context_bounds_a_single_large_message() {
        let context = vec![crate::provider::ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text("x".repeat(40_000)),
            tool_call_id: None,
            tool_calls: None,
        }];

        let reduced = reduced_context(&context);

        assert!(serde_json::to_string(&reduced).unwrap().chars().count() <= 16_000);
        assert!(matches!(
            &reduced[0].content,
            MessageContent::Text(text) if text.contains("delegated context truncated")
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
