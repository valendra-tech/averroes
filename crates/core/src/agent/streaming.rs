use super::{
    Agent, AgentRuntime, AgentStreamEvent, GovernedProvider, MAX_SILENT_PROVIDER_RETRIES,
    PROVIDER_INITIAL_RESPONSE_TIMEOUT,
};
use crate::provider::types::{FunctionCall, MessageContent, Role, ToolCall};
use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, StreamEvent};
use crate::tool::builtin::ask_user::redact_confirmation_params;
use anyhow::Result;
use futures::StreamExt;
use std::collections::HashSet;
use std::time::Duration;

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
        let (mut stream, first_event) = open_stream_with_first_event(
            &governed,
            &request,
            PROVIDER_INITIAL_RESPONSE_TIMEOUT,
            MAX_SILENT_PROVIDER_RETRIES,
        )
        .await?;
        let mut pending_event = Some(first_event);
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        let mut reasoning_started = false;
        let mut reasoning_finished = false;
        let mut announced_tool_calls = HashSet::new();

        loop {
            let event = match pending_event.take() {
                Some(event) => Some(Ok(event)),
                None => stream.next().await,
            };
            let Some(event) = event else {
                break;
            };
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
                                input: redact_confirmation_params(&input),
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

async fn open_stream_with_first_event(
    provider: &GovernedProvider,
    request: &ChatRequest,
    timeout: Duration,
    max_retries: usize,
) -> Result<(crate::provider::ChatStream, StreamEvent)> {
    for attempt in 0..=max_retries {
        let first_response = tokio::time::timeout(timeout, async {
            let mut stream = provider
                .chat_stream(request.clone())
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            while let Some(event) = stream.next().await {
                let event = event.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                if stream_event_is_response_progress(&event) {
                    return Ok(Some((stream, event)));
                }
                if matches!(event, StreamEvent::MessageEnd { .. }) {
                    return Ok(None);
                }
            }
            Ok::<_, anyhow::Error>(None)
        })
        .await;

        match first_response {
            Ok(Ok(Some(response))) => return Ok(response),
            Ok(Err(error)) => return Err(error),
            Ok(Ok(None)) | Err(_) if attempt < max_retries => {
                record_silent_retry(attempt + 1, max_retries, timeout);
            }
            Ok(Ok(None)) | Err(_) => {
                return Err(silent_provider_timeout(timeout, max_retries));
            }
        }
    }

    unreachable!("provider retry loop always returns")
}

fn stream_event_is_response_progress(event: &StreamEvent) -> bool {
    match event {
        StreamEvent::TextDelta { text } | StreamEvent::ReasoningDelta { text } => !text.is_empty(),
        StreamEvent::ToolCallDelta { .. } | StreamEvent::Error { .. } => true,
        StreamEvent::ToolCallEnd { .. }
        | StreamEvent::MessageStart { .. }
        | StreamEvent::MessageEnd { .. } => false,
    }
}

fn record_silent_retry(retry: usize, max_retries: usize, timeout: Duration) {
    crate::observability::diagnostics::record(
        crate::observability::diagnostics::DiagnosticLevel::Warning,
        "agent.request",
        format!(
            "Provider produced no response for {} seconds; cancelling and retrying ({retry}/{max_retries}).",
            timeout.as_secs()
        ),
    );
}

fn silent_provider_timeout(timeout: Duration, retries: usize) -> anyhow::Error {
    anyhow::anyhow!(
        "Provider produced no response for {} seconds after {} attempt(s)",
        timeout.as_secs(),
        retries + 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{MessageContent, Role};
    use crate::provider::ChatResponse;
    use crate::runtime::ResourceGovernor;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct SilentThenResponsiveProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for SilentThenResponsiveProvider {
        async fn chat(&self, _request: ChatRequest) -> crate::provider::Result<ChatResponse> {
            unreachable!()
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> crate::provider::Result<crate::provider::ChatStream> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(Box::new(futures::stream::pending()));
            }
            Ok(Box::new(futures::stream::iter(vec![Ok(
                StreamEvent::TextDelta {
                    text: "ready".into(),
                },
            )])))
        }

        fn context_window(&self, _model: &str) -> usize {
            128_000
        }

        fn supports_tools(&self, _model: &str) -> bool {
            true
        }

        fn default_model(&self) -> &str {
            "test"
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: "test".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("hello".into()),
                tool_call_id: None,
                tool_calls: None,
            }],
            tools: Vec::new(),
            temperature: None,
            system: None,
            reasoning_effort: None,
        }
    }

    #[tokio::test]
    async fn silent_stream_is_cancelled_and_retried_once() {
        let provider = Arc::new(SilentThenResponsiveProvider {
            calls: AtomicUsize::new(0),
        });
        let governed = GovernedProvider {
            provider: provider.clone(),
            governor: Arc::new(ResourceGovernor::new(1, 1_000)),
        };

        let (_stream, event) =
            open_stream_with_first_event(&governed, &request(), Duration::from_millis(10), 1)
                .await
                .unwrap();

        assert!(matches!(event, StreamEvent::TextDelta { text } if text == "ready"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn production_watchdog_waits_sixty_seconds_and_retries_once() {
        assert_eq!(PROVIDER_INITIAL_RESPONSE_TIMEOUT, Duration::from_secs(60));
        assert_eq!(MAX_SILENT_PROVIDER_RETRIES, 1);
    }

    #[test]
    fn transport_events_do_not_disable_the_silence_watchdog() {
        assert!(!stream_event_is_response_progress(
            &StreamEvent::MessageStart {
                message: ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text(String::new()),
                    tool_call_id: None,
                    tool_calls: None,
                },
            }
        ));
        assert!(!stream_event_is_response_progress(
            &StreamEvent::TextDelta {
                text: String::new(),
            }
        ));
        assert!(!stream_event_is_response_progress(
            &StreamEvent::MessageEnd { usage: None }
        ));
        assert!(stream_event_is_response_progress(
            &StreamEvent::ReasoningDelta {
                text: "thinking".into(),
            }
        ));
    }
}
