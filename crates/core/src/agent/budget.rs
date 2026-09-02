use super::{AgentState, ChatMessage};
use crate::provider::types::{ContentPart, MessageContent};
use crate::provider::{ChatRequest, ChatResponse, ChatStream, Provider, StreamEvent};
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
        let _permit = self.governor.acquire_call_permit().await;
        self.provider.chat(request).await
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
