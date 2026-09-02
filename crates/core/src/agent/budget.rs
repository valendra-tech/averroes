use super::{
    AgentState, ChatMessage, MAX_SILENT_PROVIDER_RETRIES, PROVIDER_INITIAL_RESPONSE_TIMEOUT,
};
use crate::provider::types::{ContentPart, MessageContent};
use crate::provider::{
    ChatRequest, ChatResponse, ChatStream, Provider, ProviderError, StreamEvent,
};
use crate::runtime::{CallPermit, ResourceGovernor};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

pub(super) fn message_text(message: &ChatMessage) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

pub(super) struct GovernedProvider {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) governor: Arc<ResourceGovernor>,
}

impl GovernedProvider {
    async fn chat_with_silent_retry(
        &self,
        request: ChatRequest,
        timeout: std::time::Duration,
        max_retries: usize,
    ) -> crate::provider::Result<ChatResponse> {
        for attempt in 0..=max_retries {
            let _permit = self.governor.acquire_call_permit().await;
            match tokio::time::timeout(timeout, self.provider.chat(request.clone())).await {
                Ok(response) => return response,
                Err(_) if attempt < max_retries => {
                    crate::observability::diagnostics::record(
                        crate::observability::diagnostics::DiagnosticLevel::Warning,
                        "agent.request",
                        format!(
                            "Provider produced no response for {} seconds; cancelling and retrying ({}/{}).",
                            timeout.as_secs(),
                            attempt + 1,
                            max_retries
                        ),
                    );
                }
                Err(_) => {
                    return Err(ProviderError::Other(format!(
                        "Provider produced no response for {} seconds after {} attempt(s)",
                        timeout.as_secs(),
                        max_retries + 1
                    )));
                }
            }
        }

        unreachable!("provider retry loop always returns")
    }
}

struct GovernedChatStream {
    inner: Option<ChatStream>,
    permit: Option<CallPermit>,
    finished: bool,
}

impl Unpin for GovernedChatStream {}

impl Stream for GovernedChatStream {
    type Item = crate::provider::Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        let poll = match this.inner.as_mut() {
            Some(inner) => Pin::new(inner).poll_next(cx),
            None => Poll::Ready(None),
        };

        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.finished = true;
                this.inner.take();
                this.permit.take();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                this.inner.take();
                this.permit.take();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Some(Ok(event))) => match &event {
                StreamEvent::MessageEnd { .. } => {
                    this.inner.take();
                    this.finished = true;
                    this.permit.take();
                    Poll::Ready(Some(Ok(event)))
                }
                StreamEvent::Error { .. } => {
                    this.finished = true;
                    this.inner.take();
                    this.permit.take();
                    Poll::Ready(Some(Ok(event)))
                }
                _ => Poll::Ready(Some(Ok(event))),
            },
        }
    }
}

pub(super) struct RunStateGuard {
    state: Arc<Mutex<AgentState>>,
    finished: bool,
}

impl RunStateGuard {
    pub(super) fn new(state: Arc<Mutex<AgentState>>) -> Self {
        Self {
            state,
            finished: false,
        }
    }

    pub(super) fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for RunStateGuard {
    fn drop(&mut self) {
        if !self.finished {
            *self.state.lock().unwrap() = AgentState::Cancelled;
        }
    }
}

#[async_trait]
impl Provider for GovernedProvider {
    async fn chat(&self, request: ChatRequest) -> crate::provider::Result<ChatResponse> {
        self.chat_with_silent_retry(
            request,
            PROVIDER_INITIAL_RESPONSE_TIMEOUT,
            MAX_SILENT_PROVIDER_RETRIES,
        )
        .await
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> crate::provider::Result<crate::provider::ChatStream> {
        let permit = self.governor.acquire_call_permit().await;
        let stream = self.provider.chat_stream(request).await?;
        Ok(Box::new(GovernedChatStream {
            inner: Some(stream),
            permit: Some(permit),
            finished: false,
        }))
    }

    fn context_window(&self, model: &str) -> usize {
        self.provider.context_window(model)
    }

    fn supports_tools(&self, model: &str) -> bool {
        self.provider.supports_tools(model)
    }

    fn default_model(&self) -> &str {
        self.provider.default_model()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{MessageContent, Role};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SilentThenResponsiveProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for SilentThenResponsiveProvider {
        async fn chat(&self, _request: ChatRequest) -> crate::provider::Result<ChatResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                futures::future::pending().await
            }
            Ok(ChatResponse {
                message: ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text("ready".into()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                reasoning: None,
                usage: None,
                stop_reason: None,
            })
        }

        async fn chat_stream(&self, _request: ChatRequest) -> crate::provider::Result<ChatStream> {
            unreachable!()
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

    #[tokio::test]
    async fn silent_non_streaming_request_is_cancelled_and_retried_once() {
        let provider = Arc::new(SilentThenResponsiveProvider {
            calls: AtomicUsize::new(0),
        });
        let governed = GovernedProvider {
            provider: provider.clone(),
            governor: Arc::new(ResourceGovernor::new(1, 1_000)),
        };
        let request = ChatRequest {
            model: "test".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            system: None,
            reasoning_effort: None,
        };

        let response = governed
            .chat_with_silent_retry(request, std::time::Duration::from_millis(10), 1)
            .await
            .unwrap();

        assert_eq!(message_text(&response.message), "ready");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
}
