use super::{Agent, AgentRuntime, AgentStreamEvent, GovernedProvider};
use crate::provider::types::{FunctionCall, MessageContent, Role, ToolCall};
use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, StreamEvent};
use anyhow::Result;
use futures::StreamExt;
use std::collections::HashSet;

impl Agent {
    pub(super) async fn chat_stream_with_events(
        &self,
        runtime: &AgentRuntime,
        request: ChatRequest,
        events: &tokio::sync::mpsc::UnboundedSender<AgentStreamEvent>,
    ) -> Result<ChatResponse> {
        let governed = GovernedProvider {
            provider: runtime.provider.clone(),
            governor: runtime.governor.clone(),
        };
        let mut stream = governed
            .chat_stream(request)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        let mut reasoning_started = false;
        let mut reasoning_finished = false;
        let mut announced_tool_calls = HashSet::new();

        while let Some(event) = stream.next().await {
            match event.map_err(|error| anyhow::anyhow!(error.to_string()))? {
                StreamEvent::TextDelta { text: delta } => {
                    if reasoning_started && !reasoning_finished {
                        let _ = events.send(AgentStreamEvent::ReasoningFinished);
                        reasoning_finished = true;
                    }
                    text.push_str(&delta);
                    let _ = events.send(AgentStreamEvent::TextDelta { text: delta });
                }
                StreamEvent::ReasoningDelta { text: delta } => {
                    if !delta.is_empty() {
                        reasoning_started = true;
                        reasoning.push_str(&delta);
                    }
                    let _ = events.send(AgentStreamEvent::ReasoningDelta { text: delta });
                }
                StreamEvent::ToolCallDelta {
                    id,
                    name,
                    arguments_delta,
                } => {
                    let inside_reasoning = reasoning_started && !reasoning_finished;
                    if id.is_empty() {
                        continue;
                    }
                    if let Some(tool_call) = tool_calls
                        .iter_mut()
                        .find(|call: &&mut ToolCall| call.id == id)
                    {
                        if !name.is_empty() {
                            tool_call.function.name = name;
                        }
                        tool_call.function.arguments.push_str(&arguments_delta);
                    } else {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            call_type: "function".into(),
                            function: FunctionCall {
                                name,
                                arguments: arguments_delta,
                            },
                        });
                    }
                    if let Some(tool_call) = tool_calls.iter().find(|call| call.id == id) {
                        if !tool_call.function.name.is_empty()
                            && announced_tool_calls.insert(id.clone())
                        {
                            let input = if tool_call.function.arguments.trim().is_empty() {
                                serde_json::Value::Object(serde_json::Map::new())
                            } else {
                                serde_json::from_str(&tool_call.function.arguments).unwrap_or_else(
                                    |_| {
                                        serde_json::Value::String(
                                            tool_call.function.arguments.clone(),
                                        )
                                    },
                                )
                            };
                            let _ = events.send(AgentStreamEvent::ToolPreparing {
                                call_id: id,
                                name: tool_call.function.name.clone(),
                                input,
                                inside_reasoning,
                            });
                        }
                    }
                }
                StreamEvent::MessageEnd {
                    usage: stream_usage,
                } => {
                    if reasoning_started && !reasoning_finished {
                        let _ = events.send(AgentStreamEvent::ReasoningFinished);
                        reasoning_finished = true;
                    }
                    usage = stream_usage;
                    break;
                }
                StreamEvent::Error { message } => {
                    if reasoning_started && !reasoning_finished {
                        let _ = events.send(AgentStreamEvent::ReasoningFinished);
                    }
                    return Err(anyhow::anyhow!(message));
                }
                StreamEvent::ToolCallEnd { .. } | StreamEvent::MessageStart { .. } => {}
            }
        }

        // A provider may close the stream without sending its final sentinel.
        // Do not leave the reasoning spoiler in its perpetual loading state.
        if reasoning_started && !reasoning_finished {
            let _ = events.send(AgentStreamEvent::ReasoningFinished);
        }

        Ok(ChatResponse {
            message: ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(text),
                tool_call_id: None,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            },
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            usage,
            stop_reason: None,
        })
    }
}
